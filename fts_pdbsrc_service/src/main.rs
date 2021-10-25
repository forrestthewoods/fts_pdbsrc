use anyhow::*;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    std::env::vars().for_each(|v| println!("cargo:warning=Envvar: {:?}", v));

    // Init logging
    init_logging()?;

    std::panic::set_hook(Box::new(|panic_info| {
        log::error!("panic occurred: [{:?}]", panic_info);
    }));

    // Run program (convert enum to anyhow::error)
    match fts_pdbsrc_service::run() {
        Ok(_) => Ok(()),
        Err(e) => {
            log::error!("Unexpected error: [{}]", e);
            bail!(e);
        }
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
            if let Ok(elapsed) = std::time::SystemTime::now().duration_since(modified) {
                if elapsed > one_week_in_seconds {
                    // Delete old logs
                    std::fs::remove_file(entry.path())?;
                }
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
        path::{Path, PathBuf},
        sync::{mpsc, Arc, Mutex},
        time::Duration,
    };
    use uuid::Uuid;

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

    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct Config {
        pub paths: Vec<ConfigPath>,
        pub log_level: simplelog::LevelFilter,
    }

    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct ConfigPath {
        pub path: PathBuf,
        pub follow_symlinks: bool,
    }

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
        if let Err(e) = run_service() {
            log::error!("Unexpected error: [{:?}]", e);
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
        log::info!("Setting service to StartPending");
        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::StartPending,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        // Determine config path
        let mut config_path = std::env::current_exe()?;
        config_path.set_file_name("fts_pdbsrc_service_config.json");

        // Read config
        let config: Config = read_config(&config_path)?;

        // Update log level
        log::set_max_level(config.log_level);

        // Create initial set of PDBs
        let pdbs = find_pdbs(&config.paths);
        let pdbs: Arc<Mutex<HashMap<Uuid, PathBuf>>> = Arc::new(Mutex::new(pdbs));

        // Watch each config filepath for changes
        let mut path_watchers = watch_paths(&config.paths, pdbs.clone());

        // Watch config file
        // When config changes, clear old watchs/pdbs and refresh
        let mut config_watcher = hotwatch::Hotwatch::new().expect("hotwatch failed to initialize!");
        let pdbs2 = pdbs.clone();
        config_watcher
            .watch(&config_path, move |event: hotwatch::Event| {
                let _ = || -> anyhow::Result<()> {
                    if let hotwatch::Event::Write(path) = event {
                        log::info!("Config file [{:?}] changed. Re-parsing log.", path);

                        // Read and parse config
                        let new_config: Config = read_config(&path)?;

                        // Update log level
                        log::set_max_level(new_config.log_level);

                        // Clear old watchers
                        path_watchers.clear();

                        // Recreate watchers
                        path_watchers = watch_paths(&new_config.paths, pdbs2.clone());

                        // Find new pdbs
                        *pdbs2.lock().unwrap() = find_pdbs(&new_config.paths);
                    }

                    Ok(())
                }();
            })
            .unwrap_or_else(|_| panic!("failed to watch [{:?}]!", &config_path));

        // Listen to connections
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
                        Message::FindPdb(uuid) => {
                            log::info!("Received request for PDB with Uuid: [{}]", uuid);

                            let search_result: Option<PathBuf> = pdb_db.lock().unwrap().get(&uuid).cloned();
                            match search_result {
                                Some(path) => {
                                    log::info!("Found path [{:?}] for uuid [{}]", path, uuid);
                                    send_message(&mut stream, Message::FoundPdb((uuid, Some(path.clone()))))?
                                }
                                None => {
                                    log::info!("Failed to find match for uuid [{}]", uuid);
                                    send_message(&mut stream, Message::FoundPdb((uuid, None)))?
                                }
                            }
                        }
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

    fn watch_paths(
        paths: &[ConfigPath],
        pdbs: Arc<Mutex<HashMap<Uuid, PathBuf>>>,
    ) -> Vec<hotwatch::Hotwatch> {
        paths
            .iter()
            .filter_map(|entry| {
                let mut hw = hotwatch::Hotwatch::new().expect("hotwatch failed to initialize!");
                let pdbs2 = pdbs.clone();
                match hw.watch(&entry.path, move |event: hotwatch::Event| {
                    // Help to detect PDB
                    let is_pdb = |path: &Path| -> bool {
                        matches!(path.extension().and_then(|os_str| os_str.to_str()), Some("pdb"))
                    };

                    // Remove PDBs that are removed or renamed (src)
                    match &event {
                        hotwatch::Event::Remove(path) | hotwatch::Event::Rename(path, _) => {
                            // Ignore non-pdbs
                            if !is_pdb(path) {
                                return;
                            }

                            // Remove PDB if it's in the db
                            let mut pdbs = pdbs2.lock().unwrap();
                            let maybe_key = pdbs
                                .iter()
                                .find_map(|(key, val)| if val == path { Some(*key) } else { None });

                            if let Some(key) = maybe_key {
                                log::info!("Detected deletion of [{:?}]", pdbs.get(&key));
                                pdbs.remove(&key);
                            }
                        }
                        _ => (), // ignore other events
                    }

                    // Add PDBs that are created, modified, or renamed (dst)
                    match &event {
                        hotwatch::Event::Create(path) | hotwatch::Event::Write(path) => {
                            // Ignore events for non-PDBs
                            if !is_pdb(path) {
                                return;
                            }

                            // PDB was created or modified, process it
                            log::info!("Detected creation or modification of [{:?}]", path);
                            if let Some((uuid, path)) = process_pdb_path(path) {
                                log::info!("Found valid PDB [{:?}] with Uuid [{}]", path, uuid);
                                pdbs2.lock().unwrap().insert(uuid, path);
                            }
                        }
                        _ => (), // Ignore other events
                    }
                }) {
                    Ok(()) => {
                        log::info!("Created watch for: [{:?}]", entry.path);
                        Some(hw)
                    }
                    Err(e) => {
                        log::warn!("Failed to watch path: [{:?}]. Error: [{:?}]", &entry.path, e);
                        None
                    }
                }
            })
            .collect()
    }

    fn read_config(config_path: &Path) -> anyhow::Result<Config> {
        log::info!("Loading config file: [{:?}]", config_path);
        let config_file = std::fs::File::open(&config_path)?;

        log::info!("Parsing config");
        let config: Config = serde_json::from_reader(&config_file)?;

        log::info!("Successfully loaded config: [{:?}]", config);
        Ok(config)
    }

    fn process_pdb_path(path: &Path) -> Option<(Uuid, PathBuf)> {
        // Ignore non-PDBs
        match path.extension().and_then(|os_str| os_str.to_str()) {
            Some("pdb") => (),
            _ => return None,
        };

        log::info!("Checking PDB file: [{:?}]", path);

        // Open PDB
        let pdbfile = File::open(path).ok()?;
        log::trace!("Opened file");
        let mut pdb = pdb::PDB::open(pdbfile).ok()?;
        log::trace!("Opened file as PDB");

        // Get srcsrv stream
        let srcsrv_stream = pdb.named_stream("srcsrv".as_bytes()).ok()?;
        let srcsrv_str: &str = std::str::from_utf8(&srcsrv_stream).ok()?;
        log::trace!("Found srcsrv stream");

        // Verify srcsrv is compatible
        if srcsrv_str.contains("VERCTRL=fts_pdbsrc") && srcsrv_str.contains("VERSION=1") {
            log::trace!("Found VERCTRL=fts_pdbsrc");

            // Extract Uuid
            let key = "FTS_PDBSTR_UUID=";
            let uuid: Uuid = srcsrv_str
                .lines()
                .find(|line| line.starts_with(key))
                .and_then(|line| Uuid::parse_str(&line[key.len()..]).ok())?;
            log::trace!("Found UUID: {}", uuid);

            // Return result
            Some((uuid, path.to_owned()))
        } else {
            log::trace!("Did not find VERCTRL=fts_pdbsrc");
            None
        }
    }

    fn process_walkdir_entry(entry: walkdir::DirEntry) -> Option<(Uuid, PathBuf)> {
        if entry.file_type().is_file() {
            process_pdb_path(entry.path())
        } else {
            None
        }
    }

    fn find_pdbs(paths: &[ConfigPath]) -> HashMap<Uuid, PathBuf> {
        log::info!("Searching for PDBs:");
        let start = std::time::Instant::now();

        let pdbs = paths
            .iter()
            .flat_map(|path_entry| {
                log::info!("Searching root entry: [{:?}]", &path_entry.path);
                walkdir::WalkDir::new(&path_entry.path)
                    .follow_links(path_entry.follow_symlinks)
                    .into_iter()
                    .filter_map(|dir_entry| process_walkdir_entry(dir_entry.unwrap()))
            })
            .collect::<HashMap<Uuid, PathBuf>>();

        log::info!("Search time [{:?}]", std::time::Instant::now() - start);
        log::info!("Found PDBs: [{:?}]", pdbs);

        pdbs
    }
}
