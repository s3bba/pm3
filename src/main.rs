mod config;
mod paths;
mod protocol;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    println!("Hello, world!");
    Ok(())
}
