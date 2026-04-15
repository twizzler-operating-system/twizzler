use crate::triple::Triple;

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building ncurses for {}", triple);
    Ok(())
}
