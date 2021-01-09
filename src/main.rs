mod addons;
mod config_defs;
mod dashboard;
mod deployment_util;
mod push_img;
mod service_util;
mod vm;
mod watch;

use clap::Clap;
use once_cell::sync::Lazy;
use std::path::PathBuf;

const ROOT: Lazy<PathBuf> = Lazy::new(|| "/home/mb/projects/k8s".into());

#[derive(Debug, Clap)]
struct ArgsK {
    #[clap(last = true)]
    args: Vec<String>,
}

#[derive(Debug, Clap)]
struct ArgsAddons {
    #[clap(long)]
    only_apply: bool,
    #[clap(long, short)]
    filter: Vec<String>,
}

#[derive(Debug, Clap)]
struct ArgsPush {
    image: String,
    #[clap(long, short)]
    name: String,
}

#[derive(Clap, Debug)]
enum Args {
    Up,
    Down,
    Dash,
    Addons(ArgsAddons),
    K(ArgsK),
    Push(ArgsPush),
}

fn load_configs() -> anyhow::Result<(vm::VmConfig,)> {
    let vmc = serde_json::from_str(&xshell::read_file(ROOT.join("etc/vm.json"))?)?;
    Ok((vmc,))
}

async fn up(only_down: bool) -> anyhow::Result<()> {
    let configs = load_configs()?;
    dbg!(&configs);
    vm::down(&configs.0)?;
    if only_down {
        return Ok(());
    }
    let state = vm::create(&configs.0).await?;
    vm::setup_soft(&state).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let fut = real_main();
    let fut = tokio_compat_02::FutureExt::compat(fut);
    fut.await
}

async fn real_main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args {
        Args::Up => up(false).await,
        Args::Down => up(true).await,
        Args::Dash => dashboard::open().await,
        Args::Addons(ArgsAddons { only_apply, filter }) => {
            addons::install(only_apply, if filter.is_empty() { None } else { Some(&filter) }).await
        }
        Args::Push(ArgsPush { image, name }) => push_img::push(&image, &name).await,
        Args::K(ArgsK { args }) => {
            let status = tokio::process::Command::new("kubectl")
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .args(args)
                .env("KUBECONFIG", ROOT.join("state/kubeconfig"))
                .status()
                .await?;
            std::process::exit(status.code().unwrap_or(-1))
        }
    }
}

fn configure_kubectl() {
    std::env::set_var("KUBECONFIG", ROOT.join("state/kubeconfig"));
}

async fn kube() -> anyhow::Result<kube::Client> {
    let kubeconfig = ROOT.join("state/kubeconfig");
    let kubeconfig = kube::config::Kubeconfig::read_from(kubeconfig)?;

    let config = kube::Config::from_custom_kubeconfig(kubeconfig, &Default::default()).await?;
    Ok(kube::Client::new(config))
}
