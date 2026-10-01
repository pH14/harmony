// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stat {
    pid: i32,
    state: char,
    group: i32,
    session: i32,
}

pub fn group_members(group: i32) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut members: Vec<i32> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| read_stat(&entry.path()))
        .filter(|stat| (stat.group == group || stat.session == group) && stat.state != 'Z')
        .map(|stat| stat.pid)
        .collect();
    members.sort_unstable();
    members
}

fn read_stat(directory: &Path) -> Option<Stat> {
    parse_stat(&std::fs::read_to_string(directory.join("stat")).ok()?)
}

fn parse_stat(text: &str) -> Option<Stat> {
    let (head, tail) = text.rsplit_once(')')?;
    let pid = head.split_once(' ')?.0.parse().ok()?;
    let mut fields = tail.split_ascii_whitespace();
    let state = fields.next()?.chars().next()?;
    let _parent = fields.next()?;
    let group = fields.next()?.parse().ok()?;
    let session = fields.next()?.parse().ok()?;
    Some(Stat {
        pid,
        state,
        group,
        session,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_with_spaces_and_parentheses() {
        assert_eq!(
            parse_stat("4242 (uml (stub) x) S 4000 4100 4100 0 -1 4194560 0"),
            Some(Stat {
                pid: 4242,
                state: 'S',
                group: 4100,
                session: 4100,
            })
        );
        assert_eq!(parse_stat("4242 (truncated"), None);
    }
}
