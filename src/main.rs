use anyhow::*;
use path_slash::PathExt;
use pdb::*;
use std::fs::{File};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use structopt::StructOpt;
use subprocess::*;
use walkdir::WalkDir;

#[derive(StructOpt, Debug)]
#[structopt(
    name = "fts_pdbsrc",
    author = "Forrest Smith <forrestthewoods@gmail.com>",
    about = "Embeds and extracts source files into PDBs"
)]
struct Opts {
    #[structopt(subcommand)]
    op: Op,
}

#[derive(StructOpt, Debug)]
enum Op {
    #[structopt(name = "embed")]
    Embed(EmbedOp),

    #[structopt(name = "extract_one")]
    ExtractOne(ExtractOneOp),

    #[structopt(name = "extract_all")]
    ExtractAll(ExtractAllOp),

    #[structopt(name = "info")]
    Info(InfoOp),

    #[structopt(name = "watch")]
    Watch(WatchOp),
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

    #[structopt(short, long, help = "Out directory to extract files")]
    outdir: String,
}

#[derive(Debug, StructOpt)]
struct ExtractAllOp {
    #[structopt(short, long, help = "Target PDB for specified operation")]
    pdb: String,
}

#[derive(Debug, StructOpt)]
struct InfoOp {
    #[structopt(short, long, help = "Target PDB for specified operation")]
    pdb: String,
}

#[derive(Debug, StructOpt)]
struct WatchOp {}

/*
fts_pdbsrc --embed --targetpdb foo
fts_pdbsrc --extract --targetpdb
*/

fn main() -> anyhow::Result<()> {
    let opt: Opts = Opts::from_args();

    match opt.op {
        Op::Embed(op) => embed(op)?,
        Op::ExtractOne(op) => extract_one(op)?,
        Op::ExtractAll(op) => extract_all(op)?,
        Op::Info(op) => info(op)?,
        Op::Watch(op) => watch(op)?,
    }

    Ok(())
}

fn embed(op: EmbedOp) -> anyhow::Result<(), anyhow::Error> {
    println!("Hello, world!");

    // Load PDB
    let pdbfile = File::open(&op.pdb)?;
    let mut pdb = pdb::PDB::open(pdbfile)?;
    let string_table = pdb.string_table()?;

    // Iterate files
    let mut filepaths: Vec<_> = Default::default();

    let di = pdb.debug_information()?;
    let mut modules = di.modules()?;
    while let Some(module) = modules.next()? {
        if let Some(module_info) = pdb.module_info(&module)? {
            let line_program = module_info.line_program()?;

            let mut file_iter = line_program.files();
            while let Some(file) = file_iter.next()? {
                let filename = string_table.get(file.name)?;

                let filename_utf8 = std::str::from_utf8(filename.as_bytes())?;
                let filepath = Path::new(filename_utf8).to_slash().unwrap();
                filepaths.push(filepath);
            }
        }
    }

    // Iterate streams
    let info = pdb.pdb_information()?;
    let stream_names = info.stream_names()?;
    stream_names
        .iter()
        .for_each(|stream_name| println!("Stream: [{}]", stream_name.name));

    // Close PDB
    drop(pdb);

    // Now iterate files
    for filepath in filepaths {
        match std::fs::File::open(&filepath) {
            Ok(_) => {
                println!("File found, adding to pdb: [{:?}]", filepath);
                let pathstr = filepath.as_str();

                if pathstr.contains("Program Files") {
                    continue;
                }

                let cmd = &[
                    "pdbstr",
                    "-w",
                    &format!("-p:{}", &op.pdb),
                    &format!("-s:/fts_pdbsrc/{}", pathstr),
                    &format!("-i:{}", pathstr),
                    //"-p:c:/temp/pdb/CrashTest.pdb",
                    //"-s:ftstest2",
                    //"-i:c:/temp/cpp/CrashTest/CrashTest.cpp"
                ];

                let mut p = Popen::create(
                    cmd,
                    PopenConfig {
                        stdout: Redirection::Pipe,
                        ..Default::default()
                    },
                )?;

                let status = p.wait()?;
                match status {
                    ExitStatus::Exited(0) => (),
                    _ => bail!(
                        "File [{:?}] encountered status [{:?}] on cmd [{:?}]",
                        filepath,
                        status,
                        cmd
                    ),
                }

                println!("Successfull executed: [{:?}]", cmd);

                /*
                let result : Result<()> = match status {
                    ExitStatus::Exited(code) => {
                        if code != 0 {
                            Err(anyhow!("File [{}] encountered status [{}] with"))
                        } else {
                            Ok(())
                        }
                    },
                    _ => {
                        Error::new(ErrorKind::Other, format!("Unexpected error: [{:?}", status))
                    }
                };
                */
            }
            Err(_) => println!("File not found, skipping: [{:?}]", filepath),
        }
    }

    println!("Goodbye cruel world!");

    Ok(())
}

fn extract_one(op: ExtractOneOp) -> anyhow::Result<()> {
    let _cmd = &[
        "pdbstr",                                                 // executable
        "-r",                                                     // read
        &format!("-p:{}", op.pdb),                                // pdb path
        &format!("-s:/fts_pdbsrc/{}", op.file),                   // filepath (as stream)
        &format!("-i:%LOCALAPPDATA%/fts/fts_pdbsrc/{}", op.file), // out file
    ];

    Ok(())
}

fn extract_all(_op: ExtractAllOp) -> anyhow::Result<()> {
    Ok(())
}

fn info(op: InfoOp) -> anyhow::Result<()> {
    // Load PDB
    let pdbfile = File::open(op.pdb)?;
    let mut pdb = pdb::PDB::open(pdbfile)?;
    let string_table = pdb.string_table()?;

    // Iterate files
    let di = pdb.debug_information()?;
    let mut modules = di.modules()?;
    while let Some(module) = modules.next()? {
        if let Some(module_info) = pdb.module_info(&module)? {
            let line_program = module_info.line_program()?;

            let mut file_iter = line_program.files();
            while let Some(file) = file_iter.next()? {
                let filename = string_table.get(file.name)?;

                let filename_utf8 = std::str::from_utf8(filename.as_bytes())?;
                let filepath = Path::new(filename_utf8);

                if std::fs::metadata(filepath).is_ok() {
                    println!("File exists: [{:?}]", filepath);
                } else {
                    println!("File not found: [{:?}]", filepath);
                }
            }
        }
    }

    // Iterate streams
    let info = pdb.pdb_information()?;
    let stream_names = info.stream_names()?;
    stream_names
        .iter()
        .for_each(|stream_name| println!("Stream: [{}]", stream_name.name));

    Ok(())
}

fn watch(_: WatchOp) -> anyhow::Result<()> {
    // Find relevant pdbs (pdbs containing srcsrv w/ VERCTRL=fts_pdbsrc
    let mut relevant_pdbs : Vec<PathBuf> = Default::default();
    for entry in WalkDir::new("c:/temp/pdb").into_iter().filter_map(|e| e.ok()) {
        // Look for files
        if !entry.file_type().is_file() {
            continue;
        }

        // Look for files that end with .pdb
        if !entry
            .file_name()
            .to_str()
            .map_or(false, |filename| filename.ends_with(".pdb"))
        {
            continue;
        }

        // Check if this pdb is an fts
        let is_fts_pdbsrc = || -> anyhow::Result<bool> {
            // Open PDB
            let pdbfile = File::open(entry.path())?;
            let mut pdb = pdb::PDB::open(pdbfile)?;
            
            // Get srcsrv stream
            let srcsrv_stream = pdb.named_stream("srcsrv".as_bytes())?;
            let srcsrv_str : &str = std::str::from_utf8(&srcsrv_stream)?;

            Ok(srcsrv_str.contains("VERCTRL=fts_pdbsrc"))
        }().unwrap_or_default();
        
        if !is_fts_pdbsrc {
            continue;
        }

        relevant_pdbs.push(entry.path().to_path_buf());
    }

    let handle_connection = |stream: &mut TcpStream| -> anyhow::Result<()> {
        let mut packet_size_buf : [u8; 2] = Default::default();
        let mut packet_buf : Vec<u8> = Vec::with_capacity(64);
        loop {
            // Read packet size
            stream.read_exact(&mut packet_size_buf)?;
            let packet_size = u16::from_ne_bytes(packet_size_buf);

            // Read packet
            let packet_slice = &mut packet_buf[..packet_size as usize];
            stream.read_exact(packet_slice)?;

            // Parse and do something with packet
        }
    };

    // Listen
    let listener = TcpListener::bind("127.0.0.1:23685")?; // port chosen randomly
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                println!("New connection: {}", stream.peer_addr().unwrap());
                std::thread::spawn(move|| {
                    let _ = handle_connection(&mut stream);
                    stream.shutdown(std::net::Shutdown::Both).unwrap();
                });
            }
            Err(e) => {
                println!("TCP listener error: {}", e);
            }
        }
    }

    Ok(())
}

/*
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
*/
