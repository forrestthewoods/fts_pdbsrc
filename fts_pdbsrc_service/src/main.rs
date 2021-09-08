#[cfg(windows)]
fn main() -> windows_service::Result<()> {
    fts_pdbsrc_service::run()
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
        sync::mpsc,
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

        // Tell the system that service is running
        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        // BEGIN DO STUFF
        std::thread::spawn(|| accept_connections());

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

    fn accept_connections() -> anyhow::Result<()> {
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

                // Verify srcsrv is compatible
                if srcsrv_str.contains("VERCTRL=fts_pdbsrc") && srcsrv_str.contains("VERSION=1") {
                    // Extract Uuid
                    let key = "FTS_PDBSTR_UUID=";
                    let uuid: Uuid = srcsrv_str
                        .lines()
                        .find(|line| line.starts_with(key))
                        .and_then(|line| Uuid::parse_str(&line[key.len()..]).ok())
                        .ok_or(anyhow!("Failed to parse Uuid.\n{}", srcsrv_str))?;

                    // Store result
                    relevant_pdbs.insert(uuid, entry.path().to_path_buf());
                } else {
                    bail!("Incompatible srcsrv.\n{}", srcsrv_str);
                }

                Ok(())
            }();
        }

        let handle_connection =
            |mut stream: &mut TcpStream, pdb_db: &HashMap<Uuid, PathBuf>| -> anyhow::Result<()> {
                loop {
                    let msg = read_message(&mut stream)?;
                    match msg {
                        Message::FindPdb(uuid) => match pdb_db.get(&uuid) {
                            Some(path) => {
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
                        let _ = handle_connection(&mut stream, &pdb_copy);
                        stream.shutdown(std::net::Shutdown::Both).unwrap();
                    });
                }
                Err(e) => println!("Error accepting listener: [{}]", e),
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
