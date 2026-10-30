//! A PostgreSQL container per image tag, held for the whole test binary.
//!
//! Include in a test binary next to `containers` and `exit_slot`:
//!
//! ```rust,ignore
//! #[path = "support/containers.rs"]
//! mod containers;
//! #[path = "support/exit_slot.rs"]
//! mod exit_slot;
//! #[path = "support/postgres.rs"]
//! mod postgres;
//! ```

use diesel::{Connection, PgConnection};
use testcontainers::{
    Container, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::SyncRunner,
};

use crate::{containers, exit_slot::ExitSlot};

/// The slot one `postgres:<tag>` container lives in until the binary exits.
pub type Slot = ExitSlot<Container<GenericImage>>;

/// A connection to the `postgres:<tag>` container in `slot`, started on
/// first use.
pub fn connect(slot: &'static Slot, tag: &str) -> PgConnection {
    let port = slot.with(
        || {
            containers::sweep_abandoned();
            GenericImage::new("postgres", tag)
                .with_exposed_port(5432.tcp())
                .with_wait_for(WaitFor::message_on_stderr(
                    "database system is ready to accept connections",
                ))
                .with_env_var("POSTGRES_PASSWORD", "postgres")
                .with_env_var("POSTGRES_USER", "postgres")
                .with_env_var("POSTGRES_DB", "postgres")
                .with_labels(containers::labels("postgres"))
                .start()
                .unwrap_or_else(|e| panic!("start postgres {tag}: {e}"))
        },
        |container| {
            container
                .get_host_port_ipv4(5432)
                .expect("get postgres port")
        },
    );
    let url = format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");
    PgConnection::establish(&url).unwrap_or_else(|e| panic!("connect to postgres {tag}: {e}"))
}
