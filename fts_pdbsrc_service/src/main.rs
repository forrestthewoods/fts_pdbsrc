use anyhow::*;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    // Init logging
    init_logging()?;

    // Run program (convert enum to anyhow::error)
    match fts_pdbsrc_service::run() {
        Ok(_) => Ok(()),
        Err(e) => bail!(e),
    }
}

fn init_logging() -> anyhow::Result<()> {
    // Find log dir
    let log_dir = dirs::data_local_dir()
        .expect("Failed to local dir")
        .join("fts/fts_pdbsrc_service/logs");
    std::fs::create_dir_all(&log_dir)?;

    // Delete old log files
    let one_week_in_seconds = std::time::Duration::from_secs(60 * 60 * 24 * 7);
    for entry in std::fs::read_dir(&log_dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            let modified = metadata.modified()?;
            match std::time::SystemTime::now().duration_since(modified) {
                Ok(elapsed) => {
                    if elapsed > one_week_in_seconds {
                        // Delete old logs
                        std::fs::remove_file(entry.path())?;
                    }
                }
                _ => (), // ignore errors
            }
        }
    }

    // Create new log file
    let local_time = chrono::Local::now();
    let new_log_path = log_dir.join(local_time.format("%Y-%m-%d--%Hh-%Mm-%Ss.log").to_string());

    // Initialize simple log
    use simplelog::*;
    CombinedLogger::init(vec![WriteLogger::new(
        LevelFilter::Trace,
        Config::default(),
        std::fs::File::create(new_log_path)?,
    )])?;

    log::info!("Created on: {}", local_time);

    Ok(())
}

#[cfg(not(windows))]
fn main() {
    panic!("This program is only intended to run on Windows.");
}

#[cfg(windows)]
mod fts_pdbsrc_service {
    use anyhow::*;
    use serde::{Deserialize, Serialize};
    use std::{
        collections::HashMap,
        ffi::OsString,
        fs::File,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        path::PathBuf,
        sync::{mpsc, Arc, Mutex},
        time::Duration,
    };
    use uuid::Uuid;
    use walkdir::WalkDir;

    use windows_service::{
        define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher, Result,
    };

    const SERVICE_NAME: &str = "fts_pdbsrc_service";
    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    pub fn run() -> Result<()> {
        log::info!("Starting service");

        // Register generated `ffi_service_main` with the system and start the service, blocking
        // this thread until the service is stopped.
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
    }

    // Generate the windows service boilerplate.
    // The boilerplate contains the low-level service entry function (ffi_service_main) that parses
    // incoming service arguments into Vec<OsString> and passes them to user defined service
    // entry (my_service_main).
    define_windows_service!(ffi_service_main, my_service_main);

    // Service entry function which is called on background thread by the system with service
    // parameters. There is no stdout or stderr at this point so make sure to configure the log
    // output to file if needed.
    pub fn my_service_main(_arguments: Vec<OsString>) {
        if let Err(_e) = run_service() {
            // Handle the error, by logging or something.
        }
    }

    pub fn run_service() -> anyhow::Result<()> {
        // Create a channel to be able to poll a stop event from the service worker loop.
        let (shutdown_tx, shutdown_rx) = mpsc::channel();

        // Define system service event handler that will be receiving service events.
        let event_handler = move |control_event| -> ServiceControlHandlerResult {
            match control_event {
                // Notifies a service to report its current status information to the service
                // control manager. Always return NoError even if not implemented.
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,

                // Handle stop
                ServiceControl::Stop => {
                    shutdown_tx.send(()).unwrap();
                    ServiceControlHandlerResult::NoError
                }

                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };

        // Register system service event handler.
        // The returned status handle should be used to report service status changes to the system.
        let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;

        // Tell the system that service is initializing itself
        log::info!("Setting service to running");
        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::StartPending,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        // Process a walkdir entry returning valid fts_pdbsrc pdbs
        let process_entry = |entry: walkdir::DirEntry| -> anyhow::Result<(Uuid, PathBuf)> {
            if entry.file_type().is_file() {
                // Open PDB
                let path = entry.path();
                let pdbfile = File::open(path)?;
                let mut pdb = pdb::PDB::open(pdbfile)?;

                // Get srcsrv stream
                let srcsrv_stream = pdb.named_stream("srcsrv".as_bytes())?;
                let srcsrv_str: &str = std::str::from_utf8(&srcsrv_stream)?;

                // Verify srcsrv is compatible
                if srcsrv_str.contains("VERCTRL=fts_pdbsrc") && srcsrv_str.contains("VERSION=1") {
                    // Extract Uuid
                    let key = "FTS_PDBSTR_UUID=";
                    let uuid: Uuid = srcsrv_str
                        .lines()
                        .find(|line| line.starts_with(key))
                        .and_then(|line| Uuid::parse_str(&line[key.len()..]).ok())
                        .ok_or(anyhow!("Failed to parse Uuid.\n{}", srcsrv_str))?;

                    // Return result
                    let path = path.to_owned();
                    Ok((uuid, path.to_owned()))
                } else {
                    bail!("Incompatible srcsrv.\n{}", srcsrv_str);
                }
            } else {
                bail!("Not a file")
            }
        };

        // TODO: get path from config
        let initial_paths: Vec<PathBuf> = vec!["c:/temp".into()];
        log::info!("Searching initial paths for PDBs. [{:?}]", initial_paths);
        let start = std::time::Instant::now();
        let pdbs = initial_paths
            .iter()
            .flat_map(|root| {
                WalkDir::new(root)
                    .into_iter()
                    .filter_map(|entry| process_entry(entry.unwrap()).ok())
            })
            .collect::<HashMap<Uuid, PathBuf>>();

        log::info!("Search time [{:?}]", std::time::Instant::now() - start);
        log::info!("Initial PDBs: [{:?}]", pdbs);

        let pdbs: Arc<Mutex<HashMap<Uuid, PathBuf>>> = Arc::new(Mutex::new(pdbs));

        // TODO: add watch via notify crate

        // BEGIN DO STUFF
        std::thread::spawn(move || accept_connections(pdbs));

        // Tell the system that service is running
        log::info!("Setting service to running");
        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        loop {
            // Poll shutdown event.
            match shutdown_rx.recv_timeout(Duration::from_secs(1)) {
                // Break the loop either upon stop or channel disconnect
                Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => break,

                // Continue work if no events were received within the timeout
                Err(mpsc::RecvTimeoutError::Timeout) => (),
            };
        }
        // END DO STUFF

        // Tell the system that service has stopped.
        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        Ok(())
    }

    // copy pasted from fts_pdbsrc/src/main.rs for simplicity
    #[derive(Serialize, Deserialize, Debug)]
    enum Message {
        FindPdb(Uuid),
        FoundPdb((Uuid, Option<PathBuf>)),
    }

    fn accept_connections(relevant_pdbs: Arc<Mutex<HashMap<Uuid, PathBuf>>>) -> anyhow::Result<()> {
        log::info!("Accepting connections");
        let handle_connection =
            |mut stream: &mut TcpStream, pdb_db: Arc<Mutex<HashMap<Uuid, PathBuf>>>| -> anyhow::Result<()> {
                loop {
                    let msg = read_message(&mut stream)?;
                    match msg {
                        Message::FindPdb(uuid) => match pdb_db.lock().unwrap().get(&uuid) {
                            Some(path) => {
                                log::trace!("Found path [{:?}] for uuid [{}]", path, uuid);
                                send_message(&mut stream, Message::FoundPdb((uuid, Some(path.clone()))))?
                            }
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
                        let _ = handle_connection(&mut stream, pdb_copy);
                        stream.shutdown(std::net::Shutdown::Both).unwrap();
                    });
                }
                Err(e) => log::warn!("Error accepting listener: [{}]", e),
            }
        }

        Ok(())
    }

    // copy pasted from fts_pdbsrc/src/main.rs for simplicity
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

    // copy pasted from fts_pdbsrc/src/main.rs for simplicity
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
}
