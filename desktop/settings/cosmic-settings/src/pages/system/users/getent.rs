// Copyright 2024 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use std::io::{BufRead, BufReader};
use std::str::FromStr;

pub fn passwd(range: (u64, u64)) -> Vec<PasswdUser> {
    let stdout = match crate::claw_glue::run_capture(&["getent", "passwd"], Some(5)) {
        Ok(stdout) => stdout,
        Err(_) => return Vec::new(),
    };

    let mut users = Vec::new();

    let mut reader = BufReader::new(stdout.as_bytes());
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            _ => (),
        }

        if let Ok(user) = line.trim().parse::<PasswdUser>()
            && user.uid >= range.0
            && user.uid <= range.1
        {
            users.push(user);
        }
    }

    users
}

pub fn group() -> Vec<Group> {
    let stdout = match crate::claw_glue::run_capture(&["getent", "group"], Some(5)) {
        Ok(stdout) => stdout,
        Err(_) => return Vec::new(),
    };

    let mut groups = Vec::new();

    let mut reader = BufReader::new(stdout.as_bytes());
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            _ => (),
        }

        if let Ok(group) = line.trim().parse::<Group>() {
            groups.push(group);
        }
    }

    groups
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Group {
    pub uid: u64,
    pub name: Box<str>,
    pub users: Vec<Box<str>>,
}

impl FromStr for Group {
    type Err = ();

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut fields = line.split(':');

        Ok(Group {
            name: fields.next().ok_or(())?.into(),
            uid: fields.nth(1).ok_or(())?.parse().map_err(|_| ())?,
            users: fields
                .next()
                .ok_or(())?
                .split(',')
                .map(Box::from)
                .collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PasswdUser {
    pub uid: u64,
    pub username: Box<str>,
    pub full_name: Box<str>,
}

impl FromStr for PasswdUser {
    type Err = ();

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut fields = line.split(':');

        Ok(PasswdUser {
            username: fields.next().ok_or(())?.into(),
            uid: fields.nth(1).ok_or(())?.parse().map_err(|_| ())?,
            full_name: fields.nth(1).ok_or(())?.split(',').next().ok_or(())?.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/pages/system/users/getent.rs"
    ));
}
