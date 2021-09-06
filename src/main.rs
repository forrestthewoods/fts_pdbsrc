use anyhow::*;
use path_slash::PathExt;
use pdb::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use structopt::StructOpt;
use subprocess::*;
use uuid::Uuid;
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

    #[structopt(short, long, parse(from_os_str), help = "Root for files to embed")]
    roots: Vec<PathBuf>,
}

#[derive(Debug, StructOpt)]
struct ExtractOneOp {
    #[structopt(short, long, help = "File to extract")]
    file: String,

    #[structopt(short, long, help = "Output path, including filename, to create")]
    out: String,
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

#[derive(Serialize, Deserialize, Debug)]
enum Message {
    FindPdb(Uuid),
    FoundPdb((Uuid, Option<PathBuf>)),
}

/*
fts_pdbsrc --embed --targetpdb foo
fts_pdbsrc --extract --targetpdb
*/

fn main() {
    // Parse args
    let opts: Opts = Opts::from_args();

    // Run program
    let exit_code = match run(opts) {
        Ok(_) => 0,
        Err(err) => {
            eprint!("Error: {:?}", err);
            1
        }
    };

    // Result result
    std::process::exit(exit_code);
}

fn run(opts: Opts) -> anyhow::Result<()> {
    match opts.op {
        Op::Embed(op) => embed(op)?,
        Op::ExtractOne(op) => extract_one(op)?,
        Op::ExtractAll(op) => extract_all(op)?,
        Op::Info(op) => info(op)?,
        Op::Watch(op) => watch(op)?,
    }

    Ok(())
}

fn embed(op: EmbedOp) -> anyhow::Result<(), anyhow::Error> {
    let canonical_roots: Vec<PathBuf> = op.roots.iter().filter_map(|root| fs::canonicalize(root).ok()).collect();

    // Load PDB
    let pdbfile = File::open(&op.pdb)?;
    let mut pdb = pdb::PDB::open(pdbfile)?;
    let string_table = pdb.string_table()?;

    // Iterate files
    let mut filepaths: Vec<(PathBuf, PathBuf)> = Default::default();

    let di = pdb.debug_information()?;
    let mut modules = di.modules()?;
    while let Some(module) = modules.next()? {
        if let Some(module_info) = pdb.module_info(&module)? {
            let line_program = module_info.line_program()?;

            let mut file_iter = line_program.files();
            while let Some(file) = file_iter.next()? {
                let raw_filepath = string_table.get(file.name)?;

                let filename_utf8 = std::str::from_utf8(raw_filepath.as_bytes())?;
                let filepath = Path::new(filename_utf8);

                if let Ok(canonical_filepath) = fs::canonicalize(&filepath) {
                    match canonical_roots
                        .iter()
                        .filter_map(|root| {
                            canonical_filepath
                                .starts_with(root)
                                .then(|| canonical_filepath.iter().skip(root.iter().count()).collect())
                        })
                        .next()
                    {
                        Some(subpath) => filepaths.push((filepath.to_owned(), subpath)),
                        None => {}
                    }
                }
            }
        }
    }

    // Close PDB so we can write to it
    drop(pdb);

    // Write source files into PDB
    for (filepath, relpath) in &filepaths {
        let cmd = &[
            "pdbstr",                                                // exe to run
            "-w",                                                    // write
            &format!("-p:{}", &op.pdb),                              // path to pdb
            &format!("-s:/fts_pdbsrc/{}", relpath.to_slash_lossy()), // stream to write
            &format!("-i:{}", filepath.to_slash_lossy()),            // file to write into stream
        ];

        run_command(cmd)?;
    }

    // Create tempfile representing srcsrv.ini
    let mut srcsrv = tempfile::NamedTempFile::new()?;
    let uuid = uuid::Uuid::new_v4();
    writeln!(srcsrv, "SRCSRV: ini ------------------------------------------------")?;
    writeln!(srcsrv, "VERSION=1")?;
    writeln!(srcsrv, "VERCTRL=fts_pdbsrc")?;
    writeln!(srcsrv, "FTS_UUID={}", uuid)?;
    writeln!(srcsrv, "SRCSRV: variables ------------------------------------------")?;
    writeln!(srcsrv, "SRCSRVTRG=%LOCALAPPDATA%/fts_pdbsrc/{}/%var2%", uuid)?;
    writeln!(
        srcsrv,
        "SRCSRVCMD=fts_pdbsrc extract_one --file %var2% --out %SRCSRVTRG%"
    )?;
    writeln!(
        srcsrv,
        "SRCSRV: source files ------------------------------------------"
    )?;

    for (filepath, relpath) in &filepaths {
        writeln!(
            srcsrv,
            "{}*{}*{}",
            filepath.to_slash_lossy(),
            relpath.to_slash_lossy(),
            filepath.file_name().unwrap().to_string_lossy()
        )?;
    }
    writeln!(srcsrv, "SRCSRV: end ------------------------------------------------")?;

    // Close and keep tempfile
    let (_, tempfile_path) = srcsrv.keep()?;

    // Write srcsrv
    let cmd = &[
        "pdbstr",                                          // exe to run
        "-w",                                              // write
        &format!("-p:{}", &op.pdb),                        // path to pdb
        &format!("-s:srcsrv"),                             // stream to write
        &format!("-i:{}", tempfile_path.to_slash_lossy()), // file to write into stream
    ];
    run_command(cmd)?;

    // Delete tempfile
    std::fs::remove_file(tempfile_path)?;

    Ok(())
}

fn extract_one(op: ExtractOneOp) -> anyhow::Result<()> {
    // Temp: span watch
    std::thread::spawn(|| watch(WatchOp {}));

    // Query server
    match TcpStream::connect("localhost:23685") {
        Ok(mut stream) => {
            // Ask service for PDB path
            let uuid = Uuid::parse_str("936DA01F9ABD4d9d80C702AF85C822A8")?;
            send_message(&mut stream, Message::FindPdb(uuid))?;

            // Wait for response
            let response = read_message(&mut stream)?;

            // Go ahead and close stream
            drop(stream);

            // Read response
            let (uuid, pdb_path) = match response {
                Message::FoundPdb((uuid, Some(path))) => (uuid, path),
                _ => {
                    return Err(anyhow!(
                        "extract_one queried service for PDB with uuid [{}], but failed with response: [{:?}]",
                        uuid,
                        response
                    ))
                }
            };

            println!("Success! [{}] [{:?}]", uuid, pdb_path);

            // Load PDB
            let pdb_file = File::open(pdb_path)?;
            let _pdb = pdb::PDB::open(pdb_file)?;
        }
        Err(e) => {
            println!("Failed to connect: {}", e);
        }
    }

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
    let mut relevant_pdbs: HashMap<Uuid, PathBuf> = Default::default();

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
        // Use lambda for conveient result catching
        let _ = || -> anyhow::Result<()> {
            // Open PDB
            let pdbfile = File::open(entry.path())?;
            let mut pdb = pdb::PDB::open(pdbfile)?;

            // Get srcsrv stream
            let srcsrv_stream = pdb.named_stream("srcsrv".as_bytes())?;
            let srcsrv_str: &str = std::str::from_utf8(&srcsrv_stream)?;

            if srcsrv_str.contains("VERCTRL=fts_pdbsrc") {
                // TODO: Find UUID
                let uuid = Uuid::parse_str("936DA01F9ABD4d9d80C702AF85C822A8")?;

                relevant_pdbs.insert(uuid, entry.path().to_path_buf());
            }

            Ok(())
        }();
    }

    let handle_connection = |mut stream: &mut TcpStream, pdb_db: &HashMap<Uuid, PathBuf>| -> anyhow::Result<()> {
        loop {
            let msg = read_message(&mut stream)?;
            match msg {
                Message::FindPdb(uuid) => match pdb_db.get(&uuid) {
                    Some(path) => send_message(&mut stream, Message::FoundPdb((uuid, Some(path.clone()))))?,
                    None => send_message(&mut stream, Message::FoundPdb((uuid, None)))?,
                },
                _ => return Err(anyhow!("Unexpected message: [{:?}]", msg)),
            }
        }
    };

    // Listen
    let listener = TcpListener::bind("localhost:23685")?; // port chosen randomly
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let pdb_copy = relevant_pdbs.clone();
                std::thread::spawn(move || {
                    let _ = handle_connection(&mut stream, &pdb_copy);
                    stream.shutdown(std::net::Shutdown::Both).unwrap();
                });
            }
            Err(e) => println!("Error accepting listener: [{}]", e),
        }
    }

    println!("No more listeners?");

    Ok(())
}

fn send_message(stream: &mut TcpStream, message: Message) -> anyhow::Result<()> {
    // Serialize message
    let buf = rmp_serde::to_vec(&message).unwrap();

    // Write packet size
    let packet_size = u16::to_ne_bytes(buf.len() as u16);
    stream.write_all(&packet_size)?;

    // Write message
    stream.write_all(&buf)?;

    Ok(())
}

fn read_message(stream: &mut TcpStream) -> anyhow::Result<Message> {
    // Read packet size
    let mut packet_size_buf: [u8; 2] = Default::default();
    stream.read_exact(&mut packet_size_buf)?;
    let packet_size = u16::from_ne_bytes(packet_size_buf);

    // Read packet
    let mut packet_buf = vec![0; packet_size as usize]; // TODO: make thread_local
    stream.read_exact(&mut packet_buf)?;

    // Deserialize
    let message: Message = rmp_serde::from_read_ref(&packet_buf)?;

    Ok(message)
}

fn run_command(cmd: &[&str]) -> anyhow::Result<()> {
    let mut p = Popen::create(
        cmd,
        PopenConfig {
            stdout: Redirection::Pipe,
            ..Default::default()
        },
    )?;

    let status = p.wait()?;
    match status {
        ExitStatus::Exited(0) => Ok(()),
        _ => bail!("Encountered status [{:?}] on cmd [{:?}]", status, cmd),
    }
}
