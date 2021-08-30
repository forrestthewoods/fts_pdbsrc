use pdb::*;
use std::result::Result;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
#[structopt(
    name = "fts_pdbsrc",
    author = "Forrest Smith <forrestthewoods@gmail.com>",
    about = "Embeds and extracts source files into PDBs"
)]
struct Opts {
    #[structopt(subcommand)]
    op : Op,
}

#[derive(StructOpt, Debug)]
enum Op {
    #[structopt(name = "embed")]
    Embed(EmbedOp),

    #[structopt(name = "extract_one")]
    ExtractOne(ExtractOneOp),

    #[structopt(name = "extract_all")]
    ExtractAll(ExtractAllOp)
}

#[derive(Debug, StructOpt)]
struct EmbedOp {
    #[structopt(short, long, help = "Target PDB for specified operation")]
    pdb: String,
}

#[derive(Debug, StructOpt)]
struct ExtractOneOp {
    #[structopt(short, long, help = "Target PDB for specified operation")]
    pdb: String,

    #[structopt(short, long, help = "Single file to extract")]
    file: String,
}   

#[derive(Debug, StructOpt)]
struct ExtractAllOp {
    #[structopt(short, long, help = "Target PDB for specified operation")]
    pdb: String,
}    

/*
fts_pdbsrc --embed --targetpdb foo
fts_pdbsrc --extract --targetpdb
*/

fn main() -> Result<(), Box<dyn std::error::Error>> {

    //println!("fts_pdbsrc");
    //println!("  CurrentDir: {}", std::env::current_dir().unwrap().to_string_lossy());

    let opt : Opts = Opts::from_args();
    println!("{:?}", opt);
    
    Ok(())


    /*
    println!("Hello, world!");

    let file = std::fs::File::open("C:/temp/pdb/CrashTest.pdb")?;
    let mut pdb = pdb::PDB::open(file)?;

    let string_table = pdb.string_table()?;
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

    let di = pdb.debug_information()?;
    let mut modules = di.modules()?;
    while let Some(module) = modules.next()? {
        if let Some(module_info) = pdb.module_info(&module)? {
            let line_program = module_info.line_program()?;
            line_program.files().for_each(|file| {
                let filename = string_table.get(file.name)?;
                println!("File: [{}]", filename);
                // println!("File: [{}]");
                // println!("File: [{:?}]", f);
                Ok(())
            })?;
        }
    }

    let info = pdb.pdb_information()?;
    let stream_names = info.stream_names()?;
    stream_names.iter().for_each(|stream_name| println!("Stream: [{}]", stream_name.name));


    println!("Goodbye cruel world!");

    Ok(())
    */
}
