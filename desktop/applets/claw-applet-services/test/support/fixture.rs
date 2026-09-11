use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct Fixture {
    pub root: PathBuf,
    original: Vec<(&'static str, Option<OsString>)>,
}

impl Fixture {
    pub fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../build/applet-provider-fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(root.join("data/calendar")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let root = root.canonicalize().unwrap();
        let original = [
            "COS_BIN",
            "COS_DATA_DIR",
            "HOME",
            "COS_APP_ID",
            "COS_APP_GUI",
            "PATH",
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect();
        fs::write(root.join("cos"), r#"#!/bin/sh
set -eu
root=${0%/*}
printf '%s\n' "$*" >> "$root/calls"
printf '%s:%s\n' "${COS_APP_ID-}" "${COS_APP_GUI-}" >> "$root/contexts"
if [ -f "$root/stderr-reply" ]; then cat "$root/stderr-reply" >&2; fi
if [ -f "$root/hang-policy" ]; then
    printf '%s\n' "$$" > "$root/blocked-policy-pid"
    exec /bin/sleep 30
fi
if [ "$1" = __policy ]; then
    if [ -f "$root/policy-reply" ]; then
        cat "$root/policy-reply"
    elif [ -f "$root/allow" ]; then
        if [ "$4" = --name ]; then
            printf '{"decision":"allow","verb":"%s","scope":{"kind":"name","value":"%s"}}\n' "$3" "$5"
        else
            printf '{"decision":"allow","verb":"%s","scope":{"kind":"wild"}}\n' "$3"
        fi
    else
        printf '%s\n' '{"decision":"deny","reason":"fixture denied"}'
    fi
elif [ -f "$root/provider-failed" ]; then
    printf '%s\n' 'fixture provider failed' >&2
    exit 13
elif [ "$1 $2" = "agent ls" ]; then
    if [ -f "$root/task-reply" ]; then
        cat "$root/task-reply"
    else
        printf '%s\n' '{"n":1,"tasks":[{"id":"task-1","purpose":"Review","status":"running","created_at":"2026-09-09"}]}'
    fi
elif [ "$1 $2" = "sys resources" ]; then
    printf '%s\n' '{"memory":{"used_mb":200,"total_mb":1000},"disk":{"used_mb":300,"total_mb":2000}}'
else
    exit 14
fi
"#).unwrap();
        fs::set_permissions(root.join("cos"), fs::Permissions::from_mode(0o700)).unwrap();
        unsafe {
            std::env::set_var("COS_BIN", root.join("cos"));
            std::env::set_var("COS_DATA_DIR", root.join("data"));
            std::env::set_var("HOME", root.join("home"));
            std::env::set_var("COS_APP_ID", "example-app-data-client");
            std::env::remove_var("COS_APP_GUI");
        }
        Self { root, original }
    }

    pub fn allow(&self) {
        fs::write(self.root.join("allow"), "").unwrap();
    }

    #[allow(dead_code)]
    pub fn binary(default: &str) -> PathBuf {
        let binary = std::env::var_os("CLAW_APPLET_TEST_PROVIDER")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(default));
        assert!(
            binary.is_absolute() && binary.is_file(),
            "invalid provider test binary: {}",
            binary.display()
        );
        binary
    }

    #[allow(dead_code)]
    pub fn provider(&self, binary: &str) -> PathBuf {
        let binary = Self::binary(binary);
        let quoted = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        let path = self.root.join("launch-provider");
        // Only this test child sees the synthetic kernel at the fixed path.
        fs::write(&path, format!(
            "#!/bin/sh\nexec /usr/bin/unshare --user --map-root-user --mount /bin/sh -eu -c '/usr/bin/mount --bind \"$1\" /usr/local/bin; shift; exec \"$@\"' applet-fixture {} {} \"$@\"\n",
            quoted(self.root.to_str().unwrap()), quoted(binary.to_str().unwrap()),
        )).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    pub fn calls(&self) -> String {
        match fs::read_to_string(self.root.join("calls")) {
            Ok(calls) => calls,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => panic!("{error}"),
        }
    }

    pub fn calendar(&self) {
        let connection =
            rusqlite::Connection::open(self.root.join("data/calendar/events.db")).unwrap();
        connection.execute_batch(
            "CREATE TABLE events (id TEXT, title TEXT, start_time TEXT, end_time TEXT, location TEXT);
             INSERT INTO events VALUES ('event-1', 'Review', '2026-09-09', '2026-09-10', 'Office');
             INSERT INTO events VALUES ('other-day', 'Later', '2026-09-10', '2026-09-11', NULL);",
        ).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for (key, value) in &self.original {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
        fs::remove_dir_all(&self.root).unwrap();
    }
}
