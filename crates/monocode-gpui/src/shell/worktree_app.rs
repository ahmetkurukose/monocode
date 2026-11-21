//! Agents' `worktrees.merge` and `worktrees.delete` (Operator threads and Monos). They run
//! the same shell paths as the chat's pill, the Worktrees tab and the Delete dialog, so
//! the rail, the chats and Settings change exactly as when the user does it.
//!
//! An agent may ask to delete the worktree its own turn runs in. Its folder cannot go
//! while the turn uses it, so that delete waits for the turn to end
//! ([`Shell::run_worktree_deletes_after_turns`]); the agent is told so.
use super::worktree_delete::{RemovalReport, RemoveRequest};
use super::worktree_merge::{MergeReport, MergeRequest};
use super::*;
use monocode_core::{
    files::{is_equal_or_inside, path_key},
    worktree::{self, RemoveOptions},
};
use serde_json::{Value, json};
use std::path::Path;

/// An agent's `worktrees.merge` / `worktrees.delete` in `project`; `caller` is the asking
/// session (an Operator thread), whose own worktree it means without a path or branch.
pub(crate) struct WorktreeCall {
    pub action: String,
    pub project: String,
    pub input: Value,
    pub caller: Option<i64>,
}

/// Where an agent's call is answered.
pub(crate) type AppReply = Box<dyn FnOnce(&mut Shell, Result<Value, String>, &mut Context<Shell>)>;

/// A delete waiting for a session's turn to end.
pub(crate) struct DeleteAfterTurn {
    pub session: i64,
    pub project: String,
    pub path: String,
    pub force: bool,
    /// Its branch was merged into this one: delete it even if `branch -d` refuses.
    pub merged_into: Option<String>,
}

fn removal_json(path: &str, report: &RemovalReport) -> Value {
    json!({
        "path": path,
        "deleted": true,
        "branchDeleted": report.removed.branch_deleted,
        "preservedBranch": report.removed.preserved_branch,
        "chatsMovedTo": report.moved_to,
        "chatsMoved": report.moved,
        "warnings": report.removed.warnings,
    })
}

fn merge_json(report: &MergeReport) -> Value {
    let m = &report.merged;
    let mut value = json!({
        "branch": m.branch,
        "base": m.base,
        "commits": m.commits,
        "fastForward": m.fast_forward,
        "mergedIn": m.target,
        "deleted": false,
        "summary": super::worktree_merge::merge_summary(report),
    });
    if let Some(removed) = &report.removed {
        value["deleted"] = json!(true);
        value["branchDeleted"] = json!(removed.removed.branch_deleted);
        value["chatsMovedTo"] = json!(removed.moved_to);
    }
    value
}

impl Shell {
    /// `worktrees.merge {path?, branch?, delete?, force?}` and `worktrees.delete {path?,
    /// branch?, force?, deleteSessions?}` in `project`. Without a path or branch the
    /// caller's own worktree is meant. `caller` is the asking session (Operator) and
    /// the working copy it runs in.
    pub(crate) fn app_worktree_action(
        &mut self,
        call: WorktreeCall,
        reply: AppReply,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let WorktreeCall {
            action,
            project,
            input,
            caller,
        } = call;
        let caller_copy = caller.map(|id| self.session_working_copy(id, cx).to_owned());
        let text = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let flag = |key: &str| input.get(key).and_then(Value::as_bool).unwrap_or(false);
        let (path, branch) = (text("path"), text("branch"));
        let mine = (path.is_none() && branch.is_none())
            .then(|| caller_copy.clone())
            .flatten()
            .filter(|copy| path_key(copy) != path_key(&project));
        let target = worktree::merge::find(
            Path::new(&project),
            path.as_deref().or(mine.as_deref()),
            branch.as_deref(),
        );
        let target = match target {
            Ok(target) => target.to_string_lossy().into_owned(),
            Err(error) => return reply(self, Err(error), cx),
        };
        // The caller's own turn runs in it: its folder may only go once the turn ends.
        let own = caller_copy
            .as_deref()
            .is_some_and(|copy| is_equal_or_inside(copy, &target));
        let force = flag("force");
        let after_turn = |this: &mut Shell, session: i64| {
            this.worktree_ui.after_turn.push(DeleteAfterTurn {
                session,
                project: project.clone(),
                path: target.clone(),
                force,
                merged_into: None,
            });
        };
        match action.as_str() {
            "worktrees.merge" => {
                let delete = flag("delete");
                let later = (delete && own).then_some(caller).flatten();
                let (later_project, later_path) = (project.clone(), target.clone());
                self.merge_worktree(
                    MergeRequest {
                        project: project.clone(),
                        path: target.clone(),
                        delete: delete && later.is_none(),
                        force,
                        caller,
                        done: Some(Box::new(move |this, result, _, cx| {
                            let result = result.map(|report| {
                                let mut value = merge_json(&report);
                                if let Some(session) = later {
                                    this.worktree_ui.after_turn.push(DeleteAfterTurn {
                                        session,
                                        project: later_project,
                                        path: later_path,
                                        force,
                                        merged_into: Some(report.merged.base.clone()),
                                    });
                                    value["deleteAfterTurn"] = json!(true);
                                    value["note"] = json!(
                                        "Merged. The worktree is deleted when this turn ends, and this chat then continues on the base branch."
                                    );
                                }
                                value
                            });
                            reply(this, result, cx)
                        })),
                    },
                    window,
                    cx,
                );
            }
            "worktrees.delete" => {
                if own && let Some(session) = caller {
                    after_turn(self, session);
                    cx.notify();
                    return reply(
                        self,
                        Ok(json!({
                            "path": target,
                            "deleted": false,
                            "deleteAfterTurn": true,
                            "note": "This turn runs in that worktree, so it is deleted when the turn ends; this chat then continues on the branch it came from.",
                        })),
                        cx,
                    );
                }
                let report_path = target.clone();
                self.remove_worktree_with(
                    RemoveRequest {
                        cwd: project,
                        path: target,
                        delete_sessions: flag("deleteSessions"),
                        opts: RemoveOptions {
                            force,
                            ..RemoveOptions::default()
                        },
                        merged_into: None,
                        done: Some(Box::new(move |this, result, _, cx| {
                            reply(
                                this,
                                result.map(|report| removal_json(&report_path, &report)),
                                cx,
                            )
                        })),
                    },
                    window,
                    cx,
                );
            }
            _ => reply(self, Err("Unknown worktree action".into()), cx),
        }
    }

    /// Deletes asked for by a session's turn in that worktree, once the turn has ended
    /// (a session that is gone no longer holds it). Errors show in the window.
    pub(crate) fn run_worktree_deletes_after_turns(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.worktree_ui.after_turn.is_empty() {
            return;
        }
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.worktree_ui.after_turn)
            .into_iter()
            .partition(|d| {
                !self
                    .sessions
                    .get(&d.session)
                    .is_some_and(|view| view.read(cx).busy)
            });
        self.worktree_ui.after_turn = waiting;
        for delete in ready {
            self.remove_worktree_with(
                RemoveRequest {
                    cwd: delete.project,
                    path: delete.path,
                    delete_sessions: false,
                    opts: RemoveOptions {
                        force: delete.force,
                        ..RemoveOptions::default()
                    },
                    merged_into: delete.merged_into,
                    done: None,
                },
                window,
                cx,
            );
        }
    }
}
