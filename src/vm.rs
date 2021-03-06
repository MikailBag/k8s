use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Deserialize, Debug)]
pub struct VmConfig {
    #[serde(default = "VmConfig::default_vm_name")]
    name: String,
}

impl VmConfig {
    fn default_vm_name() -> String {
        "k8s".to_string()
    }
}

#[derive(Serialize, Deserialize)]
pub struct VmState {
    name: String,
    pub ip: String,
    priv_ip: String,
}

pub struct Sess(openssh::Session);

impl Sess {
    async fn print_all<R: tokio::io::AsyncRead>(r: R, b: Arc<tokio::sync::Barrier>) {
        tokio::pin!(r);
        let mut buf = Vec::with_capacity(256);
        loop {
            buf.clear();
            match r.read_buf(&mut buf).await {
                Err(e) => eprintln!("io error: {}", e),
                Ok(0) => break,
                _ => print!("{}", String::from_utf8_lossy(&buf)),
            }
        }
        b.wait().await;
    }

    pub async fn run(&mut self, args: &[&str]) -> anyhow::Result<()> {
        {
            let mut s = '$'.to_string();
            for arg in args {
                s.push(' ');
                s.push_str(arg);
            }
            println!("{}", s);
        }
        let mut cmd = self.0.command(args[0]);
        cmd.raw_args(args.iter().skip(1));
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        let mut child = cmd.spawn().context("failed to spawn")?;

        let barrier = Arc::new(tokio::sync::Barrier::new(3));

        tokio::task::spawn(Self::print_all(
            child.stdout().take().unwrap(),
            barrier.clone(),
        ));
        tokio::task::spawn(Self::print_all(
            child.stderr().take().unwrap(),
            barrier.clone(),
        ));

        let status = child.wait().await?;
        if !status.success() {
            anyhow::bail!("Child process failed: code {:?}", status.code());
        }
        barrier.wait().await;

        Ok(())
    }

    pub async fn send(&mut self, path: &str, data: &[u8]) -> anyhow::Result<()> {
        let mut sftp = self.0.sftp();
        let mut remote_file = sftp
            .write_to(path)
            .await
            .context("failed to open remote file for writing")?;
        remote_file
            .write_all(data)
            .await
            .context("failed to write data")?;
        remote_file.close().await.context("finalization error")?;
        Ok(())
    }

    pub async fn pull(&mut self, path: &str) -> anyhow::Result<Vec<u8>> {
        let mut sftp = self.0.sftp();
        let mut remote_file = sftp
            .read_from(path)
            .await
            .context("failed to open remote file for reading")?;
        let mut data = Vec::new();
        remote_file
            .read_to_end(&mut data)
            .await
            .context("failed to read data")?;
        remote_file.close().await.context("finalization error")?;
        Ok(data)
    }
}

impl VmState {
    pub async fn connect(&self) -> anyhow::Result<Sess> {
        openssh::Session::connect(format!("yc-user@{}", self.ip), openssh::KnownHosts::Accept)
            .await
            .map_err(Into::into)
            .map(Sess)
    }
}

const MAX_ATTEMPTS: usize = 6;
pub fn down(config: &VmConfig) -> anyhow::Result<()> {
    let vm_name = &config.name;
    xshell::cmd!("yc compute instance delete --name {vm_name}")
        .run()
        .ok();

    Ok(())
}

pub async fn create(config: &VmConfig) -> anyhow::Result<VmState> {
    let vm_name = &config.name;

    let cmd = xshell::cmd!(
        "yc compute instance create 
    --name {vm_name}
    --zone ru-central1-a
    --public-ip
    --preemptible
    --service-account-name nobody
    --cores 2
    --core-fraction 20
    --memory 8g
    --create-boot-disk image-folder-id=standard-images,image-family=ubuntu-2004-lts,size=100
    --ssh-key /home/mb/.ssh/id_rsa.pub"
    );
    cmd.run()?;

    let vm_desc =
        xshell::cmd!("yc --format json-rest compute instance get --name {vm_name}").read()?;
    let vm_desc: serde_json::Value = serde_json::from_str(&vm_desc)?;

    let state = VmState {
        name: config.name.clone(),
        ip: vm_desc
            .pointer("/networkInterfaces/0/primaryV4Address/oneToOneNat/address")
            .context("bad pointer or response")?
            .as_str()
            .context("wtf not string")?
            .to_string(),
        priv_ip: vm_desc
            .pointer("/networkInterfaces/0/primaryV4Address/address")
            .context("bad pointer or response")?
            .as_str()
            .context("wtf not string")?
            .to_string(),
    };

    println!("Waiting for VM to become ready");
    for attempt in 0..=MAX_ATTEMPTS {
        println!("Attempt {}/{}", attempt + 1, MAX_ATTEMPTS);
        let res = async {
            let mut sess = state.connect().await?;

            sess.run(&["echo", "OK"]).await
        }
        .await;
        if res.is_ok() {
            println!("VM is ready now!");
            break;
        }
        if let Err(err) = res {
            if attempt == MAX_ATTEMPTS {
                anyhow::bail!("Deadline exceeded");
            }
            eprintln!("Error (will retry): {:#}", err);
            tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
        }
    }

    println!("Saving VM state");

    let state_str = serde_json::to_string(&state)?;
    tokio::fs::write(crate::ROOT.join("state/vm.json"), state_str).await?;

    Ok(state)
}

pub async fn setup_soft(state: &VmState) -> anyhow::Result<()> {
    println!("Establishing connection to vm");
    let mut sess = state.connect().await?;
    println!("Updating apt index");
    sess.run(&["sudo", "apt-get", "update"]).await?;
    println!("Installing packages");
    sess.run(&[
        "sudo",
        "apt-get",
        "install",
        "-y",
        "apt-transport-https",
        "ca-certificates",
        "curl",
        "software-properties-common",
        "gnupg2",
    ])
    .await?;
    println!("Trusting local CA");
    let ca_settings: crate::config_defs::CaSettings =
        serde_json::from_slice(&tokio::fs::read(crate::ROOT.join("etc/ca.json")).await?)?;
    let ca_certificate = tokio::fs::read(&ca_settings.certificate).await?;
    sess.send("/tmp/ca-cert", &ca_certificate).await?;
    sess.run(&[
        "sudo",
        "cp",
        "/tmp/ca-cert",
        "/usr/local/share/ca-certificates/local-ca.crt",
    ])
    .await?;
    sess.run(&["sudo", "update-ca-certificates"]).await?;
    /*println!("Adding Docker GPG key");
    sess.run(&[
        "curl",
        "-fsSL",
        "https://download.docker.com/linux/ubuntu/gpg",
        "|",
        "sudo",
        "apt-key",
        "add",
        "-",
    ])
    .await?;*/
    println!("Adding Kubernetes GPG key");
    sess.run(&[
        "curl",
        "-fsSL",
        "https://packages.cloud.google.com/apt/doc/apt-key.gpg",
        "|",
        "sudo",
        "apt-key",
        "add",
        "-",
    ])
    .await?;
    println!("Adding Kubernetes APT repository");
    sess.run(&[
        "sudo",
        "add-apt-repository",
        "\"deb https://apt.kubernetes.io kubernetes-xenial main\"",
    ])
    .await?;
    /*
    println!("Adding Docker APT repository");
    sess.run(&[
        "sudo",
        "add-apt-repository",
        "\"deb [arch=amd64] https://download.docker.com/linux/ubuntu focal stable\"",
    ])
    .await?;*/

    println!("Updating APT db again");
    sess.run(&["sudo", "apt-get", "update"]).await?;
    println!("Preparing node for containerd");
    {
        let modules_load_config = r#"
overlay
br_netfilter        
        "#;
        let modules_load_path = "/etc/modules-load.d/containerd.conf";
        let tmp_path = "/tmp/containerd-modload";
        sess.send(tmp_path, modules_load_config.as_bytes()).await?;
        sess.run(&["sudo", "cp", tmp_path, modules_load_path])
            .await?;
        sess.run(&["sudo", "modprobe", "overlay"]).await?;
        sess.run(&["sudo", "modprobe", "br_netfilter"]).await?;
    }
    {
        let sysctls_config = r#"
net.bridge.bridge-nf-call-iptables  = 1
net.ipv4.ip_forward                 = 1
net.bridge.bridge-nf-call-ip6tables = 1        
        "#;
        let sysctls_path = "/etc/sysctl.d/99-kubernetes-cri.conf";
        let tmp_path = "/tmp/containerd-modload";
        sess.send(tmp_path, sysctls_config.as_bytes()).await?;
        sess.run(&["sudo", "cp", tmp_path, sysctls_path]).await?;
        sess.run(&["sudo", "sysctl", "--system"]).await?;
    }
    println!("Installing containerd");
    sess.run(&["sudo", "apt-get", "install", "-y", "containerd"])
        .await?;
    println!("Configuring containerd");
    sess.run(&["sudo", "mkdir", "/etc/containerd"]).await?;
    sess.run(&[
        "sudo",
        "containerd",
        "config",
        "default",
        "|",
        "sudo",
        "tee",
        "/etc/containerd/config.toml",
    ])
    .await?;
    sess.run(&["sudo", "systemctl", "restart", "containerd"])
        .await?;

    println!("Installing Kubernetes");
    sess.run(&[
        "sudo", "apt-get", "install", "-y", "kubelet", "kubeadm", "kubectl",
    ])
    .await?;
    println!("Disabling swap");
    sess.run(&["sudo", "swapoff", "-a"]).await?;
    println!("Loading k8s images");
    sess.run(&["sudo", "kubeadm", "config", "images", "pull"])
        .await?;
    println!("Pushing kubeadm config");
    let config_data = tokio::fs::read_to_string(crate::ROOT.join("etc/kubeadm.yaml"))
        .await?
        .replace("__PUB_IP__", &state.ip);
    sess.send("/tmp/kubeadm.yaml", config_data.as_bytes())
        .await?;

    println!("Running kubeadm init");
    sess.run(&["sudo", "kubeadm", "init", "--config", "/tmp/kubeadm.yaml"])
        .await?;
    println!("Configuring master kubectl");
    sess.run(&["mkdir", "-p", "/home/yc-user/.kube"]).await?;
    sess.run(&[
        "sudo",
        "cp",
        "-i",
        "/etc/kubernetes/admin.conf",
        "/home/yc-user/.kube/config",
    ])
    .await?;
    sess.run(&[
        "sudo",
        "chown",
        "$(id -u):$(id -g)",
        "/home/yc-user/.kube/config",
    ])
    .await?;
    println!("Allowing master to execute pods");
    sess.run(&[
        "kubectl",
        "taint",
        "nodes",
        "--all",
        "node-role.kubernetes.io/master-",
    ])
    .await?;
    sess.run(&[
        "kubectl",
        "annotate",
        "nodes",
        "--all",
        &format!("d-k8s.io/public-ip={}", state.ip),
    ])
    .await?;
    println!("Installing cilium");
    sess.run(&["kubectl", "create", "-f", "https://raw.githubusercontent.com/cilium/cilium/1.9.0/install/kubernetes/quick-install.yaml"]).await?;
    println!("Rescaling cilium");
    sess.run(&[
        "kubectl",
        "scale",
        "-n",
        "kube-system",
        "--replicas=1",
        "deployments/cilium-operator",
    ])
    .await?;
    println!("Downloading kubeconfig");
    let kubeconfig = sess.pull("/home/yc-user/.kube/config").await?;
    let kubeconfig =
        String::from_utf8(kubeconfig)?.replace(&state.priv_ip.to_string(), &state.ip.to_string());
    let kubeconfig_path = crate::ROOT.join("state/kubeconfig");
    tokio::fs::write(&kubeconfig_path, kubeconfig).await?;
    println!("Adding to ~/.kube/config");
    let global_kc_path = dirs::home_dir()
        .context("home dir not found")?
        .join(".kube/config");
    {
        let _e = xshell::pushenv(
            "KUBECONFIG",
            format!("{}:{}", kubeconfig_path.display(), global_kc_path.display()),
        );
        let merged = xshell::cmd!("kubectl config view --flatten").read()?;
        xshell::write_file(global_kc_path, merged)?;
    }
    Ok(())
}

pub async fn vm_ip() -> anyhow::Result<String> {
    let vm_state: crate::vm::VmState =
        serde_json::from_slice(&tokio::fs::read(crate::ROOT.join("state/vm.json")).await?)?;
    Ok(vm_state.ip)
}
