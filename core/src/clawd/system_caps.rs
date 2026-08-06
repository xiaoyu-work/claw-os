use crate::caps::{Cap, CapSet, Scope, Verb};

pub fn readonly_task_caps() -> CapSet {
    let mut caps = CapSet::new();

    for verb in [Verb::FS_READ, Verb::FS_META, Verb::FS_WATCH] {
        caps.insert(Cap::new(verb, Scope::path("/**")));
    }

    for verb in [
        Verb::PROC_OBSERVE,
        Verb::SYS_OBSERVE,
        Verb::DATA_KV_READ,
        Verb::DATA_DB_READ,
        Verb::DATA_LOG_READ,
        Verb::DATA_INBOX_READ,
        Verb::AGENT_OBSERVE,
        Verb::IPC_SUBSCRIBE,
        Verb::UI_NOTIFY,
        Verb::TIME_DELAY,
        Verb::BROWSER_TABS_READ,
    ] {
        caps.insert(Cap::new(verb, Scope::Wild));
    }

    for verb in [
        Verb::AI_CHAT,
        Verb::AI_CHAT_UNTRUSTED,
        Verb::AI_EMBED,
        Verb::AI_VISION_ANALYZE,
    ] {
        caps.insert(Cap::new(verb, Scope::name("**")));
    }

    for app in [
        "accessibility-manager",
        "audio-manager",
        "backup-center",
        "bluetooth-manager",
        "browser-attached",
        "camera-manager",
        "crash-doctor",
        "container-manager",
        "config-editor",
        "clipboard-manager",
        "db",
        "desktop-manager",
        "display-manager",
        "doc",
        "docs",
        "event-center",
        "firewall-manager",
        "fs",
        "hardware-center",
        "kv",
        "launcher",
        "log",
        "netdiag",
        "network-manager",
        "pkg",
        "power-manager",
        "printer-manager",
        "search",
        "security-center",
        "storage-manager",
        "summarize",
        "systemd",
        "system-snapshot",
        "usb-guard",
        "user-manager",
        "web",
    ] {
        caps.insert(Cap::new(Verb::AGENT_INVOKE, Scope::name(app)));
    }

    {
        let host = "**";
        caps.insert(Cap::new(Verb::BROWSER_DOM_READ, Scope::host(host)));
    }

    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_task_caps_do_not_allow_mutation() {
        let caps = readonly_task_caps();
        assert!(caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/hosts"))));
        assert!(caps.covers(&Cap::new(Verb::SYS_OBSERVE, Scope::name("packages"))));
        assert!(caps.covers(&Cap::new(Verb::AI_CHAT, Scope::name("claude"))));
        assert!(!caps.covers(&Cap::new(Verb::FS_WRITE, Scope::path("/tmp/x"))));
        assert!(!caps.covers(&Cap::new(Verb::FS_DELETE, Scope::path("/tmp/x"))));
        assert!(!caps.covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("git"))));
        assert!(!caps.covers(&Cap::new(Verb::SYS_SERVICE, Scope::name("sshd"))));
        assert!(!caps.covers(&Cap::new(Verb::SECRET_READ, Scope::name("OPENAI_API_KEY"))));
        assert!(!caps.covers(&Cap::new(Verb::NET_DIAL, Scope::host("example.com:443"))));
    }
}
