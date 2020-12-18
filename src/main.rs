mod vm;

use clap::Clap;
use once_cell::sync::Lazy;
use std::path::PathBuf;

const ROOT: Lazy<PathBuf> = Lazy::new(|| "/home/mb/projects/k8s".into());

#[derive(Debug, Clap)]
struct ArgsK {
    #[clap(last = true)]
    args: Vec<String>,
}

#[derive(Clap, Debug)]
enum Args {
    Up,
    Down,
    K(ArgsK),
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
    let args = Args::parse();
    match args {
        Args::Up => up(false).await,
        Args::Down => up(true).await,
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
