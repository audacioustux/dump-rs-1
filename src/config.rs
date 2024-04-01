use clap::Parser;
use once_cell::sync::Lazy;

#[derive(clap::Parser, Debug)]
pub struct Config {
    // Token - used to protect against
    #[clap(long, env, default_value = "secret")]
    pub token: String,
    #[clap(long, env, default_value = "80")]
    pub port: u16,
    #[clap(long, env)]
    pub card_number: String,
    #[clap(long, env)]
    pub card_name: String,
    #[clap(long, env)]
    pub card_month: String,
    #[clap(long, env)]
    pub card_year: String,
    #[clap(long, env)]
    pub card_cvv: String,
    #[clap(long, env)]
    pub default_email: String,
}

pub static CONFIG: Lazy<Config> = Lazy::new(Config::parse);
