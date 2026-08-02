// Copyright 2025 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::fl;
use cosmic::dialog::file_chooser;
use cosmic::iced::Length;
use cosmic::widget::{self, icon};
use cosmic::{Apply, Element};
use pwhash::{bcrypt, md5_crypt, sha256_crypt, sha512_crypt};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::future::Future;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use url::Url;
use zbus_polkit::policykit1::CheckAuthorizationFlags;

const DEFAULT_ICON_FILE: &str = "/usr/share/pixmaps/faces/pop-robot.png";
const USERS_ADMIN_POLKIT_POLICY_ID: &str = "com.clawos.Settings.Users.Admin";
const USER_TRANSACTION_PATH: &str =
    ".config/cosmic-initial-setup-user-transaction.json";

#[derive(Debug, Deserialize, Serialize)]
struct UserTransaction {
    username: String,
    uid: Option<u32>,
}

fn transaction_path() -> Result<PathBuf, String> {
    #[allow(deprecated)]
    let home = std::env::home_dir().ok_or("HOME is unavailable")?;
    Ok(home.join(USER_TRANSACTION_PATH))
}

fn load_transaction() -> Result<Option<UserTransaction>, String> {
    let path = transaction_path()?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect {}: {error}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "user transaction state is not a regular file: {}",
            path.display()
        ));
    }
    if metadata.len() == 0 || metadata.len() > 4096 {
        return Err("user transaction state has an invalid size".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        #[cfg(target_os = "linux")]
        let current_uid = fs::metadata("/proc/self")
            .map_err(|error| format!("inspect current uid: {error}"))?
            .uid();
        if metadata.mode() & 0o077 != 0
            || {
                #[cfg(target_os = "linux")]
                {
                    metadata.uid() != current_uid
                }
                #[cfg(not(target_os = "linux"))]
                {
                    false
                }
            }
        {
            return Err(format!(
                "user transaction state is accessible by another user: {}",
                path.display()
            ));
        }
    }
    serde_json::from_slice(
        &fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?,
    )
    .map(Some)
    .map_err(|error| format!("decode {}: {error}", path.display()))
}

fn write_transaction(transaction: &UserTransaction) -> Result<(), String> {
    let path = transaction_path()?;
    let parent = path.parent().ok_or("transaction path has no parent")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let body = serde_json::to_vec(transaction)
        .map_err(|error| format!("encode user transaction: {error}"))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let temp = path.with_extension(format!("tmp.{}.{nonce}", std::process::id()));
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| format!("create {}: {error}", temp.display()))?
    };
    #[cfg(not(unix))]
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|error| format!("create {}: {error}", temp.display()))?;
    let result = file
        .write_all(&body)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&temp, &path))
        .and_then(|_| fs::File::open(parent)?.sync_all());
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(format!("persist {}: {error}", path.display()));
    }
    Ok(())
}

fn clear_transaction() -> Result<(), String> {
    let path = transaction_path()?;
    match fs::remove_file(&path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                fs::File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|error| format!("sync {}: {error}", parent.display()))?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove {}: {error}", path.display())),
    }
}

fn user_has_working_overlay(username: &str, expected_uid: Option<u32>) -> bool {
    let Some(user) = pwd::Passwd::iter().find(|user| user.name == username) else {
        return false;
    };
    if expected_uid.is_some_and(|uid| uid != user.uid)
        || user.uid < 1000
        || user.uid >= 65534
    {
        return false;
    }
    let home = Path::new(&*user.dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(metadata) = fs::metadata(home) else {
            return false;
        };
        if !metadata.is_dir() || metadata.uid() != user.uid {
            return false;
        }
    }
    let Ok(config) = fs::read_to_string("/etc/default/cos-home") else {
        return false;
    };
    let configured = config.lines().any(|line| {
        line.trim()
            .strip_prefix("COS_HOME=")
            .map(str::trim)
            .is_some_and(|value| value == home.to_string_lossy().as_ref())
    });
    if !configured {
        return false;
    }
    Command::new("/usr/bin/findmnt")
        .args(["-n", "-o", "TARGET,FSTYPE", "--mountpoint"])
        .arg(home)
        .output()
        .is_ok_and(|output| {
            if !output.status.success() {
                return false;
            }
            let output = String::from_utf8_lossy(&output.stdout);
            let mut fields = output.split_whitespace();
            fields
                .next()
                .is_some_and(|target| target == home.to_string_lossy().as_ref())
                && fields
                    .next()
                    .is_some_and(|fs_type| matches!(fs_type, "overlay" | "overlayfs"))
        })
}

pub fn accept_committed_transaction(username: &str) -> bool {
    match load_transaction() {
        Ok(None) => user_has_working_overlay(username, None),
        Ok(Some(transaction))
            if transaction.username == username
                && user_has_working_overlay(&transaction.username, transaction.uid) =>
        {
            clear_transaction().is_ok()
        }
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorField {
    FullName,
    Username,
    Password,
    PasswordConfirm,
}

#[derive(Clone, Debug)]
pub struct Page {
    default_icon: icon::Handle,
    profile_icon: Option<icon::Handle>,
    profile_icon_path: PathBuf,
    password: String,
    password_label: String,
    password_confirm: String,
    password_confirm_label: String,
    username: String,
    username_label: String,
    full_name: String,
    fullname_label: String,
    password_hidden: bool,
    password_confirm_hidden: bool,
    user_info_complete: bool,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            default_icon: icon::from_path(PathBuf::from(DEFAULT_ICON_FILE)),
            password_label: fl!("password"),
            password_confirm_label: fl!("password-confirm"),
            username_label: fl!("create-account-page", "user-name"),
            fullname_label: fl!("create-account-page", "full-name"),
            profile_icon: None,
            profile_icon_path: DEFAULT_ICON_FILE.into(),
            username: String::new(),
            full_name: String::new(),
            password: String::new(),
            password_confirm: String::new(),
            password_hidden: true,
            password_confirm_hidden: true,
            user_info_complete: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Edit(EditorField, String),
    Applied(Result<(), String>),
    SelectedProfileImage(Arc<Result<Url, file_chooser::Error>>),
    SelectProfileImage,
    TogglePasswordConfirmVisibility,
    TogglePasswordVisibility,
}

impl From<Message> for super::Message {
    fn from(message: Message) -> Self {
        super::Message::User(message)
    }
}

impl From<Message> for crate::Message {
    fn from(message: Message) -> Self {
        crate::Message::PageMessage(message.into())
    }
}

impl super::Page for Page {
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn title(&self) -> String {
        fl!("create-account-page")
    }

    fn completed(&self) -> bool {
        self.user_info_complete
    }

    fn view(&self) -> Element<'_, super::Message> {
        let profile_image_selector = {
            let profile_icon_handle = self
                .profile_icon
                .clone()
                .unwrap_or_else(|| self.default_icon.clone());

            widget::button::icon(profile_icon_handle)
                .extra_large()
                .icon_size(103)
                .padding(0)
                .on_press(super::Message::User(Message::SelectProfileImage))
                .apply(widget::container)
                .center_x(Length::Fill)
        };

        let full_name_input = widget::container(
            widget::text_input("", &self.full_name)
                .label(&self.fullname_label)
                .on_input(|value| Message::Edit(EditorField::FullName, value).into()),
        );

        let username_input = widget::column::with_capacity(2)
            .push(
                widget::text_input("", &self.username)
                    .label(&self.username_label)
                    .on_input(|value| Message::Edit(EditorField::Username, value).into()),
            )
            .push(widget::text::caption(fl!(
                "create-account-page",
                "user-name-note"
            )));

        let password_input = widget::container(
            widget::secure_input(
                "",
                &self.password,
                Some(Message::TogglePasswordVisibility.into()),
                self.password_hidden,
            )
            .label(&self.password_label)
            .on_input(|value| Message::Edit(EditorField::Password, value).into()),
        );

        let password_confirm_input = widget::container(
            widget::secure_input(
                "",
                &self.password_confirm,
                Some(Message::TogglePasswordConfirmVisibility.into()),
                self.password_confirm_hidden,
            )
            .label(&self.password_confirm_label)
            .on_input(|value| Message::Edit(EditorField::PasswordConfirm, value).into()),
        );

        widget::column::with_capacity(6)
            .push(profile_image_selector)
            .push(full_name_input)
            .push(username_input)
            .push(password_input)
            .push(password_confirm_input)
            .push(widget::space::vertical().height(cosmic::theme::spacing().space_s))
            .spacing(cosmic::theme::spacing().space_s)
            .into()
    }

    fn apply_settings(&mut self) -> cosmic::Task<super::Message> {
        let username = self.username.clone();
        let full_name = self.full_name.clone();
        let password = self.password.clone();
        let icon_file = self
            .profile_icon_path
            .to_str()
            .unwrap_or(DEFAULT_ICON_FILE)
            .to_owned();
        let is_admin = true;

        cosmic::task::future(async move {
            super::Message::User(Message::Applied(
                create_user_transaction(username, full_name, password, icon_file, is_admin)
                    .await,
            ))
        })
    }
}

impl Page {
    pub fn update(&mut self, message: Message) -> cosmic::Task<super::Message> {
        match message {
            Message::SelectProfileImage => {
                return cosmic::task::future(async move {
                    let dialog_result = file_chooser::open::Dialog::new()
                        .title(fl!("create-account-page", "profile-add"))
                        .accept_label(fl!("create-account-page", "dialog-add"))
                        .modal(false)
                        .open_file()
                        .await
                        .map(|response| response.url().to_owned());

                    Message::SelectedProfileImage(Arc::new(dialog_result))
                });
            }

            Message::SelectedProfileImage(image_result) => {
                let url = match Arc::into_inner(image_result).unwrap() {
                    Ok(url) => url,
                    Err(why) => {
                        tracing::error!(?why, "failed to get image file");
                        return cosmic::Task::none();
                    }
                };

                let Ok(path) = url.to_file_path() else {
                    tracing::error!("selected image is not a file path");
                    return cosmic::Task::none();
                };

                self.profile_icon_path = path.clone();
                self.profile_icon = Some(icon::from_path(path));
            }

            Message::Edit(field, value) => {
                match field {
                    EditorField::FullName => {
                        self.full_name = value;

                        // Generate username based on the full name.
                        self.username.clear();
                        for char in self.full_name.chars() {
                            if char.is_alphabetic() {
                                self.username.push(char.to_ascii_lowercase());
                            }
                        }
                    }
                    EditorField::Username => {
                        if username_valid(&value) {
                            self.username = value;
                        }
                    }
                    EditorField::Password => {
                        self.password = value;
                    }
                    EditorField::PasswordConfirm => {
                        self.password_confirm = value;
                    }
                }

                self.user_info_complete = password_valid(&self.password, &self.password_confirm)
                    && username_valid(&self.username);
            }

            Message::TogglePasswordVisibility => {
                self.password_hidden = !self.password_hidden;
            }

            Message::TogglePasswordConfirmVisibility => {
                self.password_confirm_hidden = !self.password_confirm_hidden;
            }

            Message::Applied(_) => {}
        };

        cosmic::Task::none()
    }
}

async fn check_authorization(conn: &zbus::Connection) -> eyre::Result<()> {
    let proxy = zbus_polkit::policykit1::AuthorityProxy::new(conn).await?;
    let subject = zbus_polkit::policykit1::Subject::new_for_owner(std::process::id(), None, None)?;
    proxy
        .check_authorization(
            &subject,
            USERS_ADMIN_POLKIT_POLICY_ID,
            &HashMap::new(),
            CheckAuthorizationFlags::AllowUserInteraction.into(),
            "",
        )
        .await?;
    Ok(())
}

async fn request_permission_on_denial<T, Fun, Fut>(
    conn: &zbus::Connection,
    action: Fun,
) -> zbus::Result<T>
where
    Fun: Fn() -> Fut,
    Fut: Future<Output = zbus::Result<T>>,
{
    match action().await {
        Ok(value) => Ok(value),
        Err(why) => {
            if permission_was_denied(&why) {
                _ = check_authorization(conn).await;
                action().await
            } else {
                Err(why)
            }
        }
    }
}

fn permission_was_denied(result: &zbus::Error) -> bool {
    matches!(result, zbus::Error::MethodError(name, _, _) if name.as_str() == "org.freedesktop.Accounts.Error.PermissionDenied")
}

async fn create_user_transaction(
    username: String,
    full_name: String,
    password: String,
    icon_file: String,
    is_admin: bool,
) -> Result<(), String> {
    let password_hashed = hash_password(&password)?;
    let conn = zbus::Connection::system()
        .await
        .map_err(|why| format!("connect to system bus: {why}"))?;
    if recover_pending_transaction(&conn).await? {
        return Ok(());
    }
    let accounts = accounts_zbus::AccountsProxy::new(&conn)
        .await
        .map_err(|why| format!("connect to AccountsService: {why}"))?;
    if pwd::Passwd::iter().any(|entry| entry.name == username) {
        return Err(format!("user `{username}` already exists"));
    }
    write_transaction(&UserTransaction {
        username: username.clone(),
        uid: None,
    })?;
    let user_object_path = match request_permission_on_denial(&conn, || {
        accounts.create_user(&username, &full_name, if is_admin { 1 } else { 0 })
    })
    .await
    {
        Ok(path) => path,
        Err(why) => {
            return Err(format!(
                "create user `{username}`: {why}; transaction retained for recovery"
            ));
        }
    };

    let object_path = user_object_path.to_string();
    let uid = object_path
        .rsplit('/')
        .next()
        .and_then(|segment| segment.strip_prefix("User"))
        .and_then(|value| value.parse::<u32>().ok())
        .or_else(|| {
            pwd::Passwd::iter()
                .find(|entry| entry.name == username)
                .map(|entry| entry.uid)
        })
        .ok_or_else(|| {
            format!(
                "created user `{username}` but could not determine its uid for verification"
            )
        })?;
    if let Err(why) = write_transaction(&UserTransaction {
        username: username.clone(),
        uid: Some(uid),
    }) {
        return Err(rollback_created_user(&conn, &username, uid, why).await);
    }
    if !(1000..65534).contains(&uid) {
        return Err(
            rollback_created_user(
                &conn,
                &username,
                uid,
                format!("created user `{username}` with unexpected uid {uid}"),
            )
            .await,
        );
    }

    let user = match accounts_zbus::UserProxy::new(&conn, user_object_path).await {
        Ok(user) => user,
        Err(why) => {
            return Err(
                rollback_created_user(
                    &conn,
                    &username,
                    uid,
                    format!("open created user `{username}`: {why}"),
                )
                .await,
            );
        }
    };

    if let Err(why) = user.set_password(&password_hashed, "").await {
        return Err(
            rollback_created_user(
                &conn,
                &username,
                uid,
                format!("set password for `{username}`: {why}"),
            )
            .await,
        );
    }
    if let Err(why) = user.set_account_type(if is_admin { 1 } else { 0 }).await {
        return Err(
            rollback_created_user(
                &conn,
                &username,
                uid,
                format!("set account type for `{username}`: {why}"),
            )
            .await,
        );
    }
    if let Err(why) = user.set_icon_file(&icon_file).await {
        tracing::warn!(?why, %icon_file, "failed to set optional profile icon");
    }

    let mut client = match crate::greeter::GreeterProxy::new(&conn).await {
        Ok(client) => client,
        Err(why) => {
            return Err(
                rollback_created_user(
                    &conn,
                    &username,
                    uid,
                    format!("connect to greeter daemon: {why}"),
                )
                .await,
            );
        }
    };
    if let Err(why) = client.initial_setup_end(username.clone()).await {
        return Err(rollback_after_overlay_attempt(
            &conn,
            &username,
            uid,
            format!("configure home overlay for `{username}`: {why}"),
        )
        .await);
    }
    clear_transaction()
}

async fn recover_pending_transaction(conn: &zbus::Connection) -> Result<bool, String> {
    let Some(transaction) = load_transaction()? else {
        return Ok(false);
    };
    if !username_valid(&transaction.username) {
        return Err("stored user transaction has an invalid username".to_string());
    }
    if user_has_working_overlay(&transaction.username, transaction.uid) {
        clear_transaction()?;
        return Ok(true);
    }

    let user = pwd::Passwd::iter().find(|user| user.name == transaction.username);
    let Some(user) = user else {
        clear_transaction()?;
        return Ok(false);
    };
    let Some(expected_uid) = transaction.uid else {
        return Err(format!(
            "incomplete transaction for `{}` has no recorded uid; refusing destructive recovery",
            transaction.username
        ));
    };
    if expected_uid != user.uid || user.uid < 1000 || user.uid >= 65534 {
        return Err(format!(
            "refusing recovery for `{}` because uid {} does not match the transaction",
            transaction.username, user.uid
        ));
    }
    let mut greeter = crate::greeter::GreeterProxy::new(conn)
        .await
        .map_err(|why| format!("connect to greeter daemon for recovery: {why}"))?;
    greeter
        .initial_setup_abort(transaction.username.clone())
        .await
        .map_err(|why| format!("rollback home overlay for recovery: {why}"))?;
    delete_created_user(conn, &transaction.username, user.uid).await?;
    clear_transaction()?;
    Ok(false)
}

async fn delete_created_user(
    conn: &zbus::Connection,
    username: &str,
    uid: u32,
) -> Result<(), String> {
    if !(1000..65534).contains(&uid) {
        return Err(format!("refusing to delete unexpected system uid {uid}"));
    }
    let matches_transaction = pwd::Passwd::iter()
        .any(|user| user.uid == uid && user.name == username);
    if !matches_transaction {
        return Err(format!(
            "refusing to delete uid {uid}: it no longer belongs to `{username}`"
        ));
    }
    let accounts = accounts_zbus::AccountsProxy::new(conn)
        .await
        .map_err(|why| why.to_string())?;
    request_permission_on_denial(conn, || accounts.delete_user(uid as i64, true))
        .await
        .map_err(|why| why.to_string())
}

async fn rollback_after_overlay_attempt(
    conn: &zbus::Connection,
    username: &str,
    uid: u32,
    cause: String,
) -> String {
    let abort = async {
        let mut greeter = crate::greeter::GreeterProxy::new(conn)
            .await
            .map_err(|why| why.to_string())?;
        greeter
            .initial_setup_abort(username.to_string())
            .await
            .map_err(|why| why.to_string())
    }
    .await;
    if let Err(why) = abort {
        return format!(
            "{cause}; home-overlay rollback failed, account retained for recovery: {why}"
        );
    }
    rollback_created_user(conn, username, uid, cause).await
}

async fn rollback_created_user(
    conn: &zbus::Connection,
    username: &str,
    uid: u32,
    cause: String,
) -> String {
    if !(1000..65534).contains(&uid) {
        return format!(
            "{cause}; refusing to delete unexpected system uid {uid} during rollback"
        );
    }
    let rollback = delete_created_user(conn, username, uid).await;
    match rollback {
        Ok(()) => match clear_transaction() {
            Ok(()) => cause,
            Err(why) => format!("{cause}; clearing transaction failed: {why}"),
        },
        Err(why) => format!("{cause}; rollback of uid {uid} also failed: {why}"),
    }
}

fn hash_password(password_plain: &str) -> Result<String, String> {
    // TODO: Should we allow deprecated methods?
    match get_encrypt_method().as_str() {
        "SHA512" => sha512_crypt::hash(password_plain),
        "SHA256" => sha256_crypt::hash(password_plain),
        "MD5" => md5_crypt::hash(password_plain),
        _ => bcrypt::hash(password_plain),
    }
    .map_err(|why| format!("hash password: {why}"))
}

// TODO: In the future loading in the whole login.defs file into an object might be handy?
// For now, just grabbing what we need
fn get_encrypt_method() -> String {
    let mut value = String::new();
    // FIXME(claw): read-only enumeration for UI, not user action
    let login_defs = if let Ok(file) = File::open("/etc/login.defs") {
        file
    } else {
        return value;
    };
    let reader = BufReader::new(login_defs);

    for line in reader.lines() {
        if let Ok(line) = line
            && !line.trim().is_empty()
            && let Some(index) = line.find(|c: char| c.is_whitespace())
        {
            let key = line[0..index].trim();
            if key == "ENCRYPT_METHOD" {
                value = line[(index + 1)..].trim().to_string();
            }
        }
    }
    value
}

fn username_valid(username: &str) -> bool {
    Regex::new("^[a-z][a-z0-9-]{0,30}$")
        .unwrap()
        .is_match(username)
}

fn password_valid(password: &str, password_confirm: &str) -> bool {
    password == password_confirm && !password.is_empty() && !password_confirm.is_empty()
}
