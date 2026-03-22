use std::env;
use std::path::PathBuf;

use clap::Parser;
use colored::Colorize;

const ENV_BIND_WEB_INTERFACE: &str = "BIND_WEB_INTERFACE";
const ENV_DATABASE_PATH: &str = "DATABASE_PATH";
const ENV_ADMIN_PASSWORD: &str = "ADMIN_PASSWORD";
const ENV_READ_PASSWORD: &str = "READ_PASSWORD";
const ENV_POLL_INTERVAL: &str = "POLL_INTERVAL_MINUTES";

const DEFAULT_BIND_ADDRESS: &str = "0.0.0.0:3000";

#[derive(Debug, Parser)]
#[command(
    name = "wishlist-pulse",
    version,
    about = "Track Steam wishlist pulse and deliver updates to Telegram channels.",
    long_about = None
)]
struct CliArgs {
    #[arg(long, help = "Bind address for web interface (default: 0.0.0.0:3000)")]
    bind_web_interface: Option<String>,

    #[arg(long, help = "Path to SQLite database file")]
    database_path: Option<String>,

    #[arg(
        long,
        help = "Disable secure cookies (for local development without HTTPS)"
    )]
    insecure: bool,

    #[arg(long, help = "Steam polling interval in minutes (default: 5)")]
    poll_interval_minutes: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub bind_web_interface: String,
    pub database_path: PathBuf,
    pub admin_password: Option<String>,
    pub read_password: Option<String>,
    pub insecure: bool,
    pub poll_interval_minutes: u64,
}

const DEFAULT_POLL_INTERVAL_MINUTES: u64 = 5;

fn resolve_string(cli: Option<String>, env_name: &str) -> Result<Option<String>, String> {
    let env_val = env::var(env_name).ok();
    if cli.is_some() && env_val.is_some() {
        return Err(format!(
            "'{env_name}' is set via both CLI flag and environment variable. Use one or the other."
        ));
    }
    Ok(cli.or(env_val))
}

fn print_usage_hint() {
    eprintln!();
    eprintln!("{}", "USAGE:".cyan().bold());
    eprintln!("  wishlist-pulse [OPTIONS]");
    eprintln!();
    eprintln!("{}", "WEB INTERFACE (always started):".cyan().bold());
    eprintln!(
        "  {} <addr>     Bind address (default: {})",
        "--bind-web-interface".green(),
        DEFAULT_BIND_ADDRESS,
    );
    eprintln!();
    eprintln!("{}", "OPTIONAL:".cyan().bold());
    eprintln!(
        "  {} <path>       Path to SQLite database file",
        "--database-path".green()
    );
    eprintln!(
        "  {}              Disable secure cookies (for local dev without HTTPS)",
        "--insecure".green()
    );
    eprintln!(
        "  {} <min>  Steam polling interval in minutes (default: {})",
        "--poll-interval-minutes".green(),
        DEFAULT_POLL_INTERVAL_MINUTES,
    );
    eprintln!();
    eprintln!("{}", "ENVIRONMENT VARIABLES:".cyan().bold());
    eprintln!(
        "  {} {} {} {} {}",
        ENV_ADMIN_PASSWORD.yellow(),
        ENV_READ_PASSWORD.yellow(),
        ENV_BIND_WEB_INTERFACE.yellow(),
        ENV_DATABASE_PATH.yellow(),
        ENV_POLL_INTERVAL.yellow(),
    );
    eprintln!();
    eprintln!(
        "  {}",
        "Passwords must be set via environment variables. Other options accept CLI flags or env vars (not both).".dimmed()
    );
    eprintln!();
    eprintln!(
        "  {}",
        "Steam API key, Telegram config, watched games, and retention days".dimmed()
    );
    eprintln!("  {}", "are configured via the admin web panel.".dimmed());
    eprintln!();
    eprintln!(
        "  {}",
        "If no passwords are provided, a welcome page will be shown to set them up.".dimmed()
    );
}

impl AppConfig {
    pub fn load() -> AppConfig {
        let args = CliArgs::parse();

        match build_config(args) {
            Ok(config) => config,
            Err(err) => {
                eprintln!("{} {err}", "error:".red().bold());
                print_usage_hint();
                std::process::exit(2);
            }
        }
    }
}

fn build_config(args: CliArgs) -> Result<AppConfig, String> {
    let bind_web_interface = resolve_string(args.bind_web_interface, ENV_BIND_WEB_INTERFACE)?;
    let admin_password = env::var(ENV_ADMIN_PASSWORD).ok();
    let read_password = env::var(ENV_READ_PASSWORD).ok();

    // database_path
    let database_path_str = resolve_string(args.database_path, ENV_DATABASE_PATH)?;
    let database_path = database_path_str
        .map(PathBuf::from)
        .unwrap_or_else(crate::db::default_db_path);

    let bind_addr = bind_web_interface.unwrap_or_else(|| DEFAULT_BIND_ADDRESS.to_string());

    let poll_interval_str = resolve_string(
        args.poll_interval_minutes.map(|v| v.to_string()),
        ENV_POLL_INTERVAL,
    )?;
    let poll_interval_minutes = match poll_interval_str {
        Some(s) => s
            .parse::<u64>()
            .map_err(|_| format!("Invalid poll interval: '{s}' (must be a positive integer)"))?,
        None => DEFAULT_POLL_INTERVAL_MINUTES,
    };
    if poll_interval_minutes == 0 {
        return Err("Poll interval must be at least 1 minute".to_string());
    }

    Ok(AppConfig {
        bind_web_interface: bind_addr,
        database_path,
        admin_password,
        read_password,
        insecure: args.insecure,
        poll_interval_minutes,
    })
}
