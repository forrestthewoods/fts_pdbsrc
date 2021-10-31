use aes_gcm::aead::{Aead, NewAead};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::*;
use pdb::*;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpStream};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use structopt::StructOpt;
use subprocess::*;
use uuid::Uuid;

// ----------------------------------------------------------------------------
// Command line argument types
// ----------------------------------------------------------------------------
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
    #[structopt(name = "embed", about = "Embed all source files into PDB")]
    Embed(EmbedOp),

    #[structopt(name = "extract_one", about = "Extract single source file from PDB")]
    ExtractOne(ExtractOneOp),

    #[structopt(name = "info", about = "Dump files and streams in PDB")]
    Info(InfoOp),

    #[structopt(
        name = "install_service",
        about = "Install fts_pdbsrc_service.exe as Windows service"
    )]
    InstallService(InstallServiceOp),

    #[structopt(
        name = "uninstall_service",
        about = "Uninstall fts_pdbsrc_service.exe from Windows services"
    )]
    UninstallService(UninstallServiceOp),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum EncryptMode {
    Plaintext,
    EncryptWithRngKey,
    EncryptWithKey(String),
}

impl std::str::FromStr for EncryptMode {
    type Err = anyhow::Error;
    fn from_str(arg: &str) -> anyhow::Result<Self, Self::Err> {
        match arg {
            "plaintext" | "Plaintext" => Ok(EncryptMode::Plaintext),
            "EncryptWithRngKey" => Ok(EncryptMode::EncryptWithRngKey),
            arg => {
                let re_str = r"EncryptWithKey\(([a-fA-f0-9]{64})\)";
                let re = regex::Regex::new(re_str)?;
                let caps = re
                    .captures(arg)
                    .ok_or_else(|| anyhow!("Failed to regex [{}] against arg [{}]", re_str, arg))?;
                let hex_key = caps
                    .get(1)
                    .ok_or_else(|| anyhow!("Failed to get capture group [{}]", arg))?
                    .as_str();
                Ok(EncryptMode::EncryptWithKey(hex_key.to_owned()))
            }
        }
    }
}


#[derive(Debug, StructOpt)]
struct EmbedOp {
    #[structopt(short, long, help = "Target PDB for specified operation")]
    pdb: String,

    #[structopt(short, long, parse(from_os_str), help = "Root for files to embed")]
    roots: Vec<PathBuf>,

    #[structopt(
        long,
        parse(try_from_str),
        help = "Specify encryption mode. Plaintext, EncryptFromRngKey, EncryptWithKey(HexString)"
    )]
    encrypt_mode: EncryptMode,
}

#[derive(Debug, StructOpt)]
struct ExtractOneOp {
    #[structopt(short, long, help = "Uuid of PDB to extract from")]
    pdb_uuid: Uuid,

    #[structopt(short, long, help = "File to extract")]
    file: String,

    #[structopt(short, long, help = "Nonce used to decode")]
    nonce: Option<String>,

    #[structopt(short, long, help = "Output path, including filename, to create")]
    out: PathBuf,
}

#[derive(Debug, StructOpt)]
struct InfoOp {
    #[structopt(short, long, help = "Target PDB for specified operation")]
    pdb: String,
}

#[derive(Debug, StructOpt)]
struct InstallServiceOp {}

#[derive(Debug, StructOpt)]
struct UninstallServiceOp {}

#[derive(Serialize, Deserialize, Debug)]
enum Message {
    FindPdb(Uuid),
    FoundPdb((Uuid, Option<PathBuf>)),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Config {
    pub decode_keys: Vec<String>,
}

// ----------------------------------------------------------------------------
// Functions
// ----------------------------------------------------------------------------
fn main() -> anyhow::Result<()> {
    // Parse args
    let opts: Opts = Opts::from_args();

    // Read config
    let config = read_config();

    // Run program
    let exit_code = match run(opts, config) {
        Ok(_) => 0,
        Err(err) => {
            eprint!("Error: {:?}", err);
            1
        }
    };

    // Result result
    std::process::exit(exit_code);
}

fn read_config() -> Config {
    || -> anyhow::Result<Config> {
        let mut config_path = std::env::current_exe()?;
        config_path.set_file_name("fts_pdbsrc_config.json");
        let config_file = std::fs::File::open(&config_path)?;
        let config: Config = serde_json::from_reader(&config_file)?;
        Ok(config)
    }()
    .unwrap_or_default()
}

fn run(opts: Opts, config: Config) -> anyhow::Result<()> {
    match opts.op {
        Op::Embed(op) => embed(op)?,
        Op::ExtractOne(op) => extract_one(op, config)?,
        Op::Info(op) => info(op)?,
        Op::InstallService(op) => install_service(op)?,
        Op::UninstallService(op) => uninstall_service(op)?,
    }

    Ok(())
}

fn embed(op: EmbedOp) -> anyhow::Result<(), anyhow::Error> {
    let canonical_roots: Vec<PathBuf> = op
        .roots
        .iter()
        .filter_map(|root| fs::canonicalize(root).ok())
        .collect();

    // Load PDB
    let pdbfile = File::open(&op.pdb)?;
    let mut pdb = pdb::PDB::open(pdbfile)?;
    let string_table = pdb.string_table()?;

    // Iterate files
    let mut filepaths: Vec<(RawString, PathBuf, String)> = Default::default();

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
                    
                    // Find subpath relative to a specified root
                    let maybe_subpath = canonical_roots
                        .iter()
                        .filter_map(|root| {
                            canonical_filepath.starts_with(root).then(|| {
                                canonical_filepath
                                    .iter()
                                    .skip(root.iter().count())
                                    .collect::<PathBuf>()
                            })
                        })
                        .next();
                    
                    if let Some(subpath) = maybe_subpath {
                        filepaths.push((
                            raw_filepath,
                            subpath.clone(),
                            subpath
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .to_owned()
                                .to_string(),
                        ))
                    }
                }
            }
        }
    }

    // Make sure we found at least some files
    if filepaths.is_empty() {
        bail!("Failed to find any files");
    }

    // Print files that were found and will be embedded
    println!("Found following files:");
    filepaths.iter().for_each(|(_, filepath, _)| {
        println!("  {}", filepath.to_string_lossy());
    });

    // Close PDB so we can write to it
    drop(pdb);

    // RNG for key / nonce generation (if needed)
    let mut rng = rand::thread_rng();

    // Create cipher for encryption if specified by mode
    let (cipher, rng_key): (Option<Aes256Gcm>, Option<[u8; 32]>) = match &op.encrypt_mode {
        EncryptMode::Plaintext => (None, None),
        EncryptMode::EncryptWithRngKey => {
            // Create cipher with randomly generated key
            let mut rng = rand::thread_rng();
            let key_rng_bytes = rng.gen::<[u8; 32]>();
            let cipher = Aes256Gcm::new(Key::from_slice(&key_rng_bytes));
            (Some(cipher), Some(key_rng_bytes))
        }
        EncryptMode::EncryptWithKey(key_hex) => {
            // Create cipher from provided key
            let key = hex::decode(key_hex)?;
            let cipher = Aes256Gcm::new(Key::from_slice(&key));
            (Some(cipher), None)
        }
    };

    // Store per-file nonce
    let mut nonces: HashMap<RawString, String> = Default::default();

    // Write source files into PDB
    for (raw_filepath, relpath, _) in &filepaths {
        // Read file
        let mut file = File::open(&*raw_filepath.to_string())?;
        let mut plaintext : Vec<u8> = Default::default();
        file.read_to_end(&mut plaintext).with_context(|| format!("Error reading file: [{:?}]", raw_filepath))?;

        // Optionally encrypt file contents
        let (stream_filepath, delete_stream_file): (PathBuf, bool) = match &cipher {
            None => (PathBuf::from_str(&*raw_filepath.to_string())?, false),
            Some(cipher) => {
                // Create per-file nonce; 96-bits, unique per message
                let nonce_bytes = rng.gen::<[u8; 12]>();
                let nonce = Nonce::from_slice(&nonce_bytes); // 96-bits; unique per message

                // Encrypt text
                let encrypted_text = cipher
                    .encrypt(nonce, plaintext.as_slice())
                    .unwrap_or_else(|_| panic!("Failed to encrypt file: [{:?}]", raw_filepath));

                // Write encrypted data to temp file
                let mut encrypted_file = tempfile::NamedTempFile::new()?;
                encrypted_file.write_all(&encrypted_text)?;
                let (_, encrypted_filepath) = encrypted_file.keep()?;

                // Retain nonce
                nonces.insert(*raw_filepath, hex::encode(nonce_bytes));

                // Return path to tempfile with encrypted content
                (encrypted_filepath, true)
            }
        };

        // Invoke pdbstr to write encrypted file into pdbstr
        let cmd = &[
            "pdbstr",                                                 // exe to run
            "-w",                                                     // write
            &format!("-p:{}", &op.pdb),                               // path to pdb
            &format!("-s:/fts_pdbsrc/{}", relpath.to_string_lossy()), // stream to write
            &format!("-i:{}", stream_filepath.to_string_lossy()),     // file to write into stream
        ];
        run_command(cmd).with_context(|| format!("Cmd: {:?}", cmd))?;

        // Remove encrypted tempfile if one was created
        if delete_stream_file {
            std::fs::remove_file(stream_filepath)?;
        }
    }

    // Create tempfile representing srcsrv.ini
    let uuid = uuid::Uuid::new_v4();

    let mut srcsrv = tempfile::NamedTempFile::new()?;
    writeln!(
        srcsrv,
        "SRCSRV: ini ------------------------------------------------"
    )?;
    writeln!(srcsrv, "VERSION=1")?;
    writeln!(srcsrv, "VERCTRL=fts_pdbsrc")?;
    // TODO: add datetime
    writeln!(
        srcsrv,
        "SRCSRV: variables ------------------------------------------"
    )?;
    writeln!(srcsrv, "FTS_PDBSTR_UUID={}", uuid)?;
    writeln!(
        srcsrv,
        "SRCSRVTRG=%LOCALAPPDATA%\\fts\\fts_pdbsrc\\{}\\%FTS_PDBSTR_UUID%\\%var2%",
        Path::new(&op.pdb).file_stem().unwrap().to_str().unwrap()
    )?;
    if nonces.is_empty() {
        writeln!(
            srcsrv,
            "SRCSRVCMD=fts_pdbsrc extract_one --pdb-uuid %FTS_PDBSTR_UUID% --file %var2% --out %SRCSRVTRG%",
        )?;
    } else {
        writeln!(
            srcsrv,
            "SRCSRVCMD=fts_pdbsrc extract_one --pdb-uuid %FTS_PDBSTR_UUID% --file %var2% --out %SRCSRVTRG% --nonce %var4%",
        )?;
    }
    writeln!(
        srcsrv,
        "SRCSRV: source files ------------------------------------------"
    )?;

    for (raw_filepath, relpath, filename) in &filepaths {
        if nonces.is_empty() {
            writeln!(
                srcsrv,
                "{}*{}*{}",
                raw_filepath,
                relpath.to_string_lossy(),
                filename
            )?;
        } else {
            writeln!(
                srcsrv,
                "{}*{}*{}*{}",
                raw_filepath,
                relpath.to_string_lossy(),
                filename,
                nonces.get(raw_filepath).unwrap()
            )?;
        }
    }
    writeln!(
        srcsrv,
        "SRCSRV: end ------------------------------------------------"
    )?;

    // Close and keep tempfile
    let (_, tempfile_path) = srcsrv.keep()?;

    // Write srcsrv
    let cmd = &[
        "pdbstr",                                           // exe to run
        "-w",                                               // write
        &format!("-p:{}", &op.pdb),                         // path to pdb
        "-s:srcsrv",                                        // stream to write
        &format!("-i:{}", tempfile_path.to_string_lossy()), // file to write into stream
    ];
    run_command(cmd)?;

    // Delete tempfile
    std::fs::remove_file(tempfile_path)?;

    // Write key to console IFF it was randomly generated
    if let Some(rng_key) = rng_key {
        println!("Files encrypted. The following key MUST be saved to decrypt. DO NOT LOSE THIS KEY.");
        println!("BEGIN KEY------------------------------------------------");
        let key_hex = hex::encode(&rng_key);
        println!("{}", key_hex);
        println!("END KEY------------------------------------------------");
    }

    Ok(())
}

fn extract_one(op: ExtractOneOp, config: Config) -> anyhow::Result<()> {
    // Query server
    // FTS_TODO: make port configurable
    match TcpStream::connect("localhost:23685") {
        Ok(mut stream) => {
            // Ask service for PDB path
            send_message(&mut stream, Message::FindPdb(op.pdb_uuid))?;

            // Wait for response
            let response = read_message(&mut stream)?;

            // Go ahead and close stream
            drop(stream);

            // Read response
            let (_, pdb_path) = match response {
                Message::FoundPdb((uuid, Some(path))) => {
                    assert_eq!(
                        uuid, op.pdb_uuid,
                        "Mismatched Uuids. Requested: [{}] Found: [{}]",
                        op.pdb_uuid, uuid
                    );
                    (uuid, path)
                }
                _ => {
                    return Err(anyhow!(
                    "extract_one queried service for PDB with uuid [{}], but failed with response: [{:?}]",
                    op.pdb_uuid,
                    response
                ))
                }
            };

            // Load PDB
            let pdb_file = File::open(pdb_path)?;
            let mut pdb = pdb::PDB::open(pdb_file)?;

            // Get file stream
            let stream_name = format!("/fts_pdbsrc/{}", op.file);
            let file_stream = pdb
                .named_stream(stream_name.as_bytes())
                .unwrap_or_else(|_| panic!("Failed to find stream named [{}]", stream_name));
            let maybe_encrypted_text = file_stream.as_slice();

            // Decrypt file
            let try_decrypt = |config: Config, nonce_str: &str| -> anyhow::Result<Vec<u8>> {
                // Parse Nonce
                let nonce_bytes = hex::decode(nonce_str)?;
                let nonce = Nonce::from_slice(&nonce_bytes);

                // Try to decrypt with each key
                for hexkey in config.decode_keys {
                    let try_key = |key_hex: &str, nonce| -> anyhow::Result<Vec<u8>> {
                        let key_bytes = hex::decode(key_hex)?;
                        let key = Key::from_slice(&key_bytes);
                        let cipher = Aes256Gcm::new(key);

                        match cipher.decrypt(nonce, maybe_encrypted_text) {
                            Ok(plaintext) => Ok(plaintext),
                            Err(_) => bail!("Failed to decrypt with key"),
                        }
                    };

                    if let Ok(plaintext) = try_key(&hexkey, nonce) {
                        return Ok(plaintext);
                    }
                }

                bail!("Failed to decrypt with all keys")
            };

            // Get plaintext for maybe_encrypted_text
            let plaintext = match op.nonce {
                Some(ref nonce) => try_decrypt(config, nonce)?,
                None => maybe_encrypted_text.to_owned(),
            };

            // Write to output file
            let out_dir = op
                .out
                .parent()
                .ok_or_else(|| anyhow!("Failed to get directory for path [{:?}]", op.out))?;
            fs::create_dir_all(out_dir)?;
            let mut file = std::fs::File::create(op.out)?;
            file.write_all(&plaintext)?;
        }
        Err(e) => {
            println!("Failed to connect: {}", e);
        }
    }

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

fn install_service(_op: InstallServiceOp) -> anyhow::Result<()> {
    use std::ffi::OsString;
    use windows_service::{
        service::{ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType},
        service_manager::{ServiceManager, ServiceManagerAccess},
    };

    // Find where
    let service_exe_path =
        which::which("fts_pdbsrc_service.exe").expect("Could not find fts_pdbsrc_service.exe on PATH.");

    || -> anyhow::Result<()> {
        let manager_access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
        let service_manager = ServiceManager::local_computer(None::<&str>, manager_access)?;

        let service_info = ServiceInfo {
            name: OsString::from("fts_pdbsrc_service"),
            display_name: OsString::from("FTS PDB Source Service"),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: service_exe_path,
            launch_arguments: vec![],
            dependencies: vec![],
            account_name: None, // run as System
            account_password: None,
        };
        let service = service_manager
            .create_service(&service_info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)?;
        service.set_description("PDB scanning service for fts_pdbsrc")?;

        let start_args: Vec<std::ffi::OsString> = Default::default();
        service.start(&start_args)?;
        Ok(())
    }()
    .expect("Failed to start service. Are you running as administrator?");

    Ok(())
}

fn uninstall_service(_op: UninstallServiceOp) -> anyhow::Result<()> {
    use std::{thread, time::Duration};
    use windows_service::{
        service::{ServiceAccess, ServiceState},
        service_manager::{ServiceManager, ServiceManagerAccess},
    };

    let manager_access = ServiceManagerAccess::CONNECT;
    let service_manager = ServiceManager::local_computer(None::<&str>, manager_access)?;

    let service_access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = service_manager.open_service("fts_pdbsrc_service", service_access)?;

    let service_status = service.query_status()?;
    if service_status.current_state != ServiceState::Stopped {
        service.stop()?;
        // Wait for service to stop
        thread::sleep(Duration::from_secs(1));
    }

    service.delete()?;

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
