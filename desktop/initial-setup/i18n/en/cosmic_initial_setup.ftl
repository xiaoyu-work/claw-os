back = Back
cancel = Cancel
finish = Finish
identity = Identity
next = Next
password = Password
password-confirm = Confirm password
settings = Settings
skip = Skip
skip-setup-and-close = Skip setup and close
type-to-search = Type to search…
wifi = Wi-Fi

# Accessibility Page
accessibility-page = Accessibility setup
    .display-scaling = Display scaling
    .scale = Scale
    .scale-options = Additional scale options
    .screen-reader = Screen reader
    .interface-size = Interface size
    .magnifier = Magnifier
    .magnifier-description = Or use keyboard shortcuts:
        Super + = to zoom in, Super + - to zoom out,
        Super + scroll with your mouse

# SelectLanguagePage
select-language-page = Select a language

# SelectKeyboardLayoutPage
keyboard-layout-page = Select keyboard layout

# CreateAccountPage
create-account-page = Create your account
    .full-name = Full name
    .user-name = User name
    .user-name-note = This will be used to name your home folder, and can't be changed.
    .dialog-add = Add
    .profile-add = Choose profile image
    .invalid-username = Invalid username
    .password-mismatch = Password and confirmation must match

# LocationPage
timezone-and-location-page = Timezone and location
    .search-the-closest-major-city = Search the closest major city...
    .geonames-attribution = The list is sorted by the city population. Data: geonames.org (licensed CC-BY-4.0).

# AppearancePage
appearance-page = Personalize appearance
    .description = You can further customize accent colors and the look of your desktop in Appearance settings.

# LayoutPage
layout-page = Layout configuration
    .bottom-panel = Bottom panel
    .top-panel-and-dock = Top panel and dock
    .description = Move the panel or dock to any edge, change their size, and automatically hide them in Settings.

# SystemAppsPage
new-apps-page = New system applications
    .description = Enjoy an array of new system applications that come with the ClawOS desktop environment. Including Settings, ClawOS App Store, Files, Text Editor, and Terminal.

# NewKeyboardShortcutsPage
new-shortcuts-page = New keyboard shortcuts
    .description = Use Shift + Super + arrows, or drag with the pointer, to move windows. Take advantage of the visual hints when using automatic window tiling.

# WorkflowPage
workflow-page = Workspaces for your workflow
    .description = Float or automatically tile windows per-workspace using the tiling applet.
        You can select vertical or horizontal workspaces. You can also pin workspaces to make them static.

# LauncherPage
launcher-page = Fast and efficient
    .description = Press the Super (or Windows) key to activate the Launcher. Search and press Enter to open an app or switch to it. You can also jump to settings or system functions like suspend. Type “?” to learn about the Launcher's advanced features.

# AiPage — pick the system-wide LLM provider/model during initial setup.
# Skippable: the system can run fine without an LLM (every AI call just
# fails with "not configured").
ai-page = Configure AI
    .description = ClawOS ships with an AI agent that runs locally and shells out to the LLM provider you pick here. You can skip this and configure it later with `cos agent setup llm`.
    .provider = Provider
    .provider-description = Where to send chat requests.
    .model = Model
    .model-description = Provider-specific model identifier (e.g. `claude-sonnet-4-5`, `gpt-4o-mini`, `llama3.2:3b`).
    .api-key = API key
    .api-key-description = Optional — leave blank and add it later via `cos agent setup llm apply --api-key-stdin`.
    .azure-endpoint = Azure endpoint
    .azure-endpoint-description = Resource root URL from your Azure OpenAI portal (e.g. `https://acme.openai.azure.com/`). Do not append `/openai/deployments/…` — that path is constructed automatically from the model field, which must match your Azure deployment name.
    .azure-api-version = API version
    .azure-api-version-description = Azure REST API version (e.g. `2024-12-01-preview`). Find current versions in the Azure OpenAI docs.
    .apply-ok = Saved
    .apply-failed = Could not save
    .oauth-signin = Sign in with GitHub
    .oauth-signin-again = Sign in again
    .oauth-description = GitHub Copilot uses device-flow authorization — no API key needed. Click below, open the link, and enter the code shown.
    .oauth-instructions = Open this URL on any device, then enter the code:
    .oauth-waiting = Waiting for you to approve…
    .oauth-authorized = Signed in.
    .oauth-failed = Sign-in failed

# DriversPage

drivers-page = Graphics drivers
    .description = ClawOS already includes open-source drivers for every GPU. If an NVIDIA card is detected, you can install NVIDIA's proprietary driver here for full 3D performance and CUDA. You can also do this later with `claw-gpu-setup install`.
    .detecting = Detecting graphics hardware…
    .detected = Detected
    .none = Your graphics work out of the box — no extra driver is needed.
    .install-available = An NVIDIA GPU was found. Install the proprietary driver for full 3D acceleration and CUDA.
    .already = The proprietary NVIDIA driver is already installed.
    .wsl-ready = Your NVIDIA GPU is available through host passthrough — CUDA is ready, nothing to install.
    .wsl-missing = An NVIDIA GPU is expected but not visible. Install the NVIDIA driver on the host system.
    .unsupported-arch = The proprietary NVIDIA driver is not available for this processor architecture.
    .install = Install NVIDIA driver
    .installing = Installing the NVIDIA driver… this can take several minutes.
    .install-ok = NVIDIA driver installed. Restart to start using it.
    .install-failed = Could not install the driver

# WirelessPage

wireless-page = Get connected
    .explain = When connected, you'll get the latest system and security updates.
    .airplane-mode = Airplane mode is on
    .connect = Connect
    .connected = Connected
    .connecting = Connecting…
    .disconnect = Disconnect
    .forget = Forget
    .no-networks = No networks have been found
    .known-networks = Known networks
    .visible-networks = Visible networks

auth-dialog = Authentication required
    .wifi-description = Enter the password or encryption key. You can also connect by pressing the “WPS” button on the router.

forget-dialog = Forget this Wi-Fi network?
    .description = You'll need to enter a password again to use this Wi-Fi network in the future.

## Users

users = Users
    .desc = Authentication and user accounts.
    .admin = Admin
    .standard = Standard
