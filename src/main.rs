use pdb::*;
use std::result::Result;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Hello, world!");

    let file = std::fs::File::open("C:/source_control/fts_cache_test/x64/Release/fts_cache_test.pdb")?;
    let mut pdb = pdb::PDB::open(file)?;

    let symbol_table = pdb.global_symbols()?;
    let address_map = pdb.address_map()?;

    let mut symbols = symbol_table.iter();
    while let Some(symbol) = symbols.next()? {
        match symbol.parse() {
            Ok(pdb::SymbolData::Public(data)) if data.function => {
                // we found the location of a function!
                let rva = data.offset.to_rva(&address_map).unwrap_or_default();
                println!("{} is {}", rva, data.name);
            }
            _ => {}
        }
    }

    println!("Goodbye cruel world!");

    Ok(())
}
