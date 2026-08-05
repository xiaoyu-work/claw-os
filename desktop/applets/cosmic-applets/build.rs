use std::{env, fs, path::PathBuf};
use xdgen::{App, Context, FluentString};

fn main() {
    let ctx = Context::new("../i18n/", "desktop_entries").unwrap();
    let workspace_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cosmic-applets is a direct workspace member")
        .to_path_buf();
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                workspace_dir.join(path)
            }
        })
        .unwrap_or_else(|| workspace_dir.join("target"));
    let desktop_dir = target_dir.join("xdgen");
    fs::create_dir_all(&desktop_dir).unwrap();

    [
        (
            "com.clawos.AppList",
            "cosmic-app-list",
            "cosmic-app-list-comment",
            "cosmic-app-list-keywords",
        ),
        (
            "com.clawos.AppletA11y",
            "cosmic-applet-a11y",
            "cosmic-applet-a11y-comment",
            "cosmic-applet-a11y-keywords",
        ),
        (
            "com.clawos.AppletAgentActivity",
            "claw-applet-agent-activity",
            "claw-applet-agent-activity-comment",
            "claw-applet-agent-activity-keywords",
        ),
        (
            "com.clawos.AppletApprovalGate",
            "claw-applet-approval-gate",
            "claw-applet-approval-gate-comment",
            "claw-applet-approval-gate-keywords",
        ),
        (
            "com.clawos.AppletClipboard",
            "claw-applet-clipboard",
            "claw-applet-clipboard-comment",
            "claw-applet-clipboard-keywords",
        ),
        (
            "com.clawos.AppletWidgetRail",
            "claw-applet-widget-rail",
            "claw-applet-widget-rail-comment",
            "claw-applet-widget-rail-keywords",
        ),
        (
            "com.clawos.AppletAudio",
            "cosmic-applet-audio",
            "cosmic-applet-audio-comment",
            "cosmic-applet-audio-keywords",
        ),
        (
            "com.clawos.AppletBattery",
            "cosmic-applet-battery",
            "cosmic-applet-battery-comment",
            "cosmic-applet-battery-keywords",
        ),
        (
            "com.clawos.AppletBluetooth",
            "cosmic-applet-bluetooth",
            "cosmic-applet-bluetooth-comment",
            "cosmic-applet-bluetooth-keywords",
        ),
        (
            "com.clawos.AppletInputSources",
            "cosmic-applet-input-sources",
            "cosmic-applet-input-sources-comment",
            "cosmic-applet-input-sources-keywords",
        ),
        (
            "com.clawos.AppletMinimize",
            "cosmic-applet-minimize",
            "cosmic-applet-minimize-comment",
            "cosmic-applet-minimize-keywords",
        ),
        (
            "com.clawos.AppletNetwork",
            "cosmic-applet-network",
            "cosmic-applet-network-comment",
            "cosmic-applet-network-keywords",
        ),
        (
            "com.clawos.AppletNotifications",
            "cosmic-applet-notifications",
            "cosmic-applet-notifications-comment",
            "cosmic-applet-notifications-keywords",
        ),
        (
            "com.clawos.AppletPower",
            "cosmic-applet-power",
            "cosmic-applet-power-comment",
            "cosmic-applet-power-keywords",
        ),
        (
            "com.clawos.AppletStatusArea",
            "cosmic-applet-status-area",
            "cosmic-applet-status-area-comment",
            "cosmic-applet-status-area-keywords",
        ),
        (
            "com.clawos.AppletTiling",
            "cosmic-applet-tiling",
            "cosmic-applet-tiling-comment",
            "cosmic-applet-tiling-keywords",
        ),
        (
            "com.clawos.AppletTime",
            "cosmic-applet-time",
            "cosmic-applet-time-comment",
            "cosmic-applet-time-keywords",
        ),
        (
            "com.clawos.AppletWorkspaces",
            "cosmic-applet-workspaces",
            "cosmic-applet-workspaces-comment",
            "cosmic-applet-workspaces-keywords",
        ),
        (
            "com.clawos.PanelAppButton",
            "cosmic-panel-app-button",
            "cosmic-panel-app-button-comment",
            "cosmic-panel-app-button-keywords",
        ),
        (
            "com.clawos.PanelBrandButton",
            "claw-panel-brand-button",
            "claw-panel-brand-button-comment",
            "claw-panel-brand-button-keywords",
        ),
        (
            "com.clawos.PanelCalendarButton",
            "claw-applet-calendar",
            "claw-applet-calendar-comment",
            "claw-applet-calendar-keywords",
        ),
        (
            "com.clawos.PanelDockDivider",
            "claw-panel-dock-divider",
            "claw-panel-dock-divider-comment",
            "claw-panel-dock-divider-keywords",
        ),
        (
            "com.clawos.PanelLauncherButton",
            "cosmic-panel-launcher-button",
            "cosmic-panel-launcher-button-comment",
            "cosmic-panel-launcher-button-keywords",
        ),
        (
            "com.clawos.PanelWorkspacesButton",
            "cosmic-panel-workspaces-button",
            "cosmic-panel-workspaces-button-comment",
            "cosmic-panel-workspaces-button-keywords",
        ),
    ]
    .into_iter()
    .map(|(id, name, comment, keywords)| {
        let template_path = ["../", name, "/data/", id, ".desktop"].concat();

        let app = App::new(FluentString(name))
            .comment(FluentString(comment))
            .keywords(FluentString(keywords));

        (id, app.expand_desktop(&template_path, &ctx).unwrap())
    })
    .for_each(|(id, contents)| {
        fs::write(desktop_dir.join(format!("{id}.desktop")), contents).unwrap();
    });
}
