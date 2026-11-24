//! Run configuration groups: a named, ordered list of a project's own run
//! configurations and how to start them together. A group is not a process
//! of its own; each member is an ordinary configuration, started the way a
//! single one is. What the settings say is decided here as plain data; the
//! rules for acting on it are in `sequence.rs`.
use serde::{Deserialize, Serialize};

/// How long a port wait is given before it counts as a failure, unless the
/// user says otherwise.
pub const DEFAULT_WAIT_TIMEOUT: u32 = 60;

/// In which order a group's members are started.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StartOrder {
    /// In list order, each member waiting for the previous one's wait.
    #[default]
    Sequential,
    /// All at once; the list order and the waits do not matter.
    Parallel,
}

/// What happens when a member cannot be started: it cannot be resolved, it
/// ends before its wait is over, or its wait times out.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OnFailedStart {
    /// The members started so far are stopped too.
    StopGroup,
    /// The members started so far keep running; the rest are not started.
    #[default]
    DoNotStartRest,
    /// The member is skipped and the next one is started.
    Continue,
}

/// What happens when a member ends on its own after it was started.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OnMemberExit {
    StopGroup,
    #[default]
    LeaveOthers,
}

/// What a member waits for before the next one is started.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Wait {
    /// The next member starts as soon as this one has.
    #[default]
    None,
    Seconds(u32),
    /// A TCP port of this machine that accepts connections.
    Port(u16),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct GroupMember {
    /// The id of a configuration of the project.
    pub config: String,
    /// What the configuration was called when the group last saw it, to name
    /// a member whose configuration is gone.
    pub label: String,
    /// Only meaningful for a sequential group.
    pub wait: Wait,
    /// Seconds a port wait may take; after that the member has failed.
    pub timeout: u32,
}

impl Default for GroupMember {
    fn default() -> Self {
        Self {
            config: String::new(),
            label: String::new(),
            wait: Wait::None,
            timeout: DEFAULT_WAIT_TIMEOUT,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct RunGroup {
    /// Stable for the life of the group; given by `ProjectRuns::add_group`.
    pub id: String,
    pub name: String,
    pub order: StartOrder,
    pub on_failed_start: OnFailedStart,
    pub on_member_exit: OnMemberExit,
    /// In the order they are started.
    pub members: Vec<GroupMember>,
}

impl RunGroup {
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..Self::default()
        }
    }

    /// Adds a member at the end; false when the configuration is in the group
    /// already, since starting it twice would mean nothing.
    pub fn add_member(&mut self, config: &str, label: &str) -> bool {
        if self.members.iter().any(|member| member.config == config) {
            return false;
        }
        self.members.push(GroupMember {
            config: config.to_owned(),
            label: label.to_owned(),
            ..GroupMember::default()
        });
        true
    }

    /// Moves a member so that it ends up at `index` (clamped to the list).
    /// False when there is no member at `from`.
    pub fn move_member(&mut self, from: usize, index: usize) -> bool {
        if from >= self.members.len() {
            return false;
        }
        let member = self.members.remove(from);
        self.members.insert(index.min(self.members.len()), member);
        true
    }

    /// What is wrong with the group as it stands, as a sentence, or nothing
    /// when it can be saved and started.
    pub fn problem(&self) -> Option<String> {
        if self.name.trim().is_empty() {
            return Some("A group needs a name.".into());
        }
        for member in &self.members {
            if member.config.trim().is_empty() {
                return Some("A member names no run configuration.".into());
            }
            if member.wait == Wait::Port(0) {
                let label = member.label.trim();
                let who = if label.is_empty() { "A member" } else { label };
                return Some(format!("{who} waits for port 0, which is not a port."));
            }
        }
        None
    }

    pub(super) fn normalized(mut self) -> Self {
        self.id = self.id.trim().to_owned();
        self.name = self.name.trim().to_owned();
        let mut kept: Vec<GroupMember> = Vec::with_capacity(self.members.len());
        for mut member in std::mem::take(&mut self.members) {
            member.config = member.config.trim().to_owned();
            member.label = member.label.trim().to_owned();
            if member.config.is_empty() || kept.iter().any(|known| known.config == member.config) {
                continue;
            }
            if member.wait == Wait::Port(0) {
                member.wait = Wait::None;
            }
            member.timeout = member.timeout.max(1);
            kept.push(member);
        }
        self.members = kept;
        self
    }
}
