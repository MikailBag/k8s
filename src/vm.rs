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
    //let dest = format!("yc-user@{}", state.ip);
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
    println!("Adding Docker GPG key");
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
    .await?;
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

    println!("Adding Docker APT repository");
    sess.run(&[
        "sudo",
        "add-apt-repository",
        "\"deb [arch=amd64] https://download.docker.com/linux/ubuntu focal stable\"",
    ])
    .await?;

    println!("Updating APT db again");
    sess.run(&["sudo", "apt-get", "update"]).await?;
    println!("Installing docker");
    sess.run(&[
        "sudo",
        "apt-get",
        "install",
        "-y",
        "containerd.io=1.2.13-2",
        "docker-ce=5:19.03.11~3-0~ubuntu-$(lsb_release -cs)",
        "docker-ce-cli=5:19.03.11~3-0~ubuntu-$(lsb_release -cs)",
    ])
    .await?;
    println!("Configuring docker daemon");
    let docker_config = serde_json::to_string_pretty(&serde_json::json!({
        "exec-opts":[
            "native.cgroupdriver=systemd"
        ],
        "log-driver": "json-file",
        "log-opts": {
            "max-size": "100m"
        },
        "storage-driver": "overlay2"
    }))?;
    sess.send("/tmp/docker-daemon.json", docker_config.as_bytes())
        .await?;
    sess.run(&[
        "sudo",
        "cp",
        "/tmp/docker-daemon.json",
        "/etc/docker/daemon.json",
    ])
    .await?;
    println!("Restarting docker");
    sess.run(&[
        "sudo",
        "mkdir",
        "-p",
        "/etc/systemd/system/docker.service.d",
    ])
    .await?;
    sess.run(&["sudo", "systemctl", "daemon-reload"]).await?;
    sess.run(&["sudo", "systemctl", "restart", "docker"])
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
    tokio::fs::write(crate::ROOT.join("state/kubeconfig"), kubeconfig).await?;
    Ok(())
}
