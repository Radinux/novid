mod client;
mod dev;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use facelock_core::config::{self, Config};

#[derive(Parser)]
#[command(name = "facelock", about = "Authentification faciale IR")]
struct Cli {
    /// Compte visé (par défaut : toi).
    #[arg(short = 'U', long, global = true)]
    user: Option<String>,
    /// Parler au service de développement sur le bus de session.
    #[arg(long, global = true)]
    session: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Liste les visages enregistrés.
    List,
    /// Enregistre un visage (balayage guidé de la tête).
    Enroll {
        #[arg(short, long, default_value = "")]
        label: String,
    },
    /// Teste l'authentification comme le ferait PAM.
    Verify,
    /// Supprime un visage.
    Remove { id: String },
    /// Renomme un visage.
    Rename { id: String, label: String },
    /// Outils de développement (accès direct à la caméra).
    #[command(subcommand)]
    Dev(DevCmd),
}

#[derive(Subcommand)]
enum DevCmd {
    /// Capture éclairé / sombre / émetteur seul avec la détection dessinée.
    Snapshot {
        #[arg(short, long, default_value = "snapshot.png")]
        output: PathBuf,
        #[arg(long, default_value = config::DEFAULT_PATH)]
        config: PathBuf,
    },
    /// Mesures en direct ; --record enregistre les images brutes.
    Test {
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        #[arg(long)]
        record: Option<PathBuf>,
        #[arg(long, default_value = config::DEFAULT_PATH)]
        config: PathBuf,
    },
    /// Rejoue le pipeline sur des images enregistrées.
    Replay {
        dir: PathBuf,
        #[arg(long, default_value = config::DEFAULT_PATH)]
        config: PathBuf,
    },
}

fn current_user() -> Result<String> {
    let uid = nix::unistd::getuid();
    Ok(nix::unistd::User::from_uid(uid)?.context("unknown uid")?.name)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let user = match &cli.user {
        Some(u) => u.clone(),
        None => current_user()?,
    };
    if let Cmd::Dev(d) = &cli.cmd {
        return match d {
            DevCmd::Snapshot { output, config } => dev::snapshot(&Config::load(config)?, output),
            DevCmd::Test { seconds, record, config } => dev::test(&Config::load(config)?, &user, *seconds, record.as_deref()),
            DevCmd::Replay { dir, config } => dev::replay(&Config::load(config)?, &user, dir),
        };
    }
    let (conn, p) = client::connect(cli.session).await?;
    match &cli.cmd {
        Cmd::List => client::list(&p, &user).await,
        Cmd::Enroll { label } => client::enroll(&conn, &p, &user, label).await,
        Cmd::Verify => client::verify(&p, &user).await,
        Cmd::Remove { id } => {
            p.delete_profile(&user, id).await?;
            println!("Supprimé.");
            Ok(())
        }
        Cmd::Rename { id, label } => {
            p.rename_profile(&user, id, label).await?;
            println!("Renommé.");
            Ok(())
        }
        Cmd::Dev(_) => unreachable!(),
    }
}
