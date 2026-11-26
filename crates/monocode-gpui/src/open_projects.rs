//! The projects open in the rail, for the panels that can look across all of
//! them (Search in files and Changes) rather than only the selected checkout.
use crate::{commit_form::CommitForm, theme::*};
use gpui::{App, Global, Role, SharedString, Stateful, div, prelude::*, px};

#[derive(Default)]
pub(crate) struct OpenProjects(pub Vec<String>);
impl Global for OpenProjects {}

/// The shell calls this whenever its workspace is saved; observers hear of it
/// only when the list really changed.
pub(crate) fn publish(projects: &[String], cx: &mut App) {
    if cx
        .try_global::<OpenProjects>()
        .is_some_and(|open| open.0 == projects)
    {
        return;
    }
    cx.set_global(OpenProjects(projects.to_vec()));
}

/// The open projects that are local folders, in rail order, without repeats.
/// `current` comes first when given, so its results lead.
pub(crate) fn local_projects(cx: &App, current: Option<&str>) -> Vec<String> {
    let open = cx
        .try_global::<OpenProjects>()
        .map(|open| open.0.clone())
        .unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let key = monocode_core::files::path_key;
    for path in current
        .into_iter()
        .map(str::to_owned)
        .chain(open)
        .filter(|p| !p.is_empty() && p != "~")
    {
        if !out.iter().any(|known| key(known) == key(&path)) {
            out.push(path);
        }
    }
    out
}

pub(crate) fn project_name(path: &str) -> String {
    monocode_core::search::project_name(path)
}

/// The project folder holding `path`: the longest root that contains it.
pub(crate) fn owning_root<'a>(roots: &'a [String], path: &str) -> Option<&'a String> {
    roots
        .iter()
        .filter(|root| {
            let root = root.trim_end_matches('/');
            path == root || path.starts_with(&format!("{root}/"))
        })
        .max_by_key(|root| root.len())
}

/// "☐ All open projects": the checkbox both panels show.
pub(crate) fn toggle(id: &'static str, on: bool, count: usize) -> Stateful<gpui::Div> {
    let label: SharedString = if count > 1 {
        format!("All open projects ({count})").into()
    } else {
        "All open projects".into()
    };
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::CheckBox)
        .aria_label("All open projects")
        .aria_selected(on)
        .flex_shrink_0()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(content(0.75))
        .child(CommitForm::checkbox(
            SharedString::from(format!("{id}-box")),
            if on {
                crate::commit_form::Check::All
            } else {
                crate::commit_form::Check::None
            },
        ))
        .child(div().min_w_0().truncate().child(label))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_belongs_to_the_deepest_project_holding_it() {
        let roots = vec!["/a".to_owned(), "/a/b".to_owned(), "/ab".to_owned()];
        assert_eq!(owning_root(&roots, "/a/b/c.rs").unwrap(), "/a/b");
        assert_eq!(owning_root(&roots, "/a/c.rs").unwrap(), "/a");
        assert_eq!(owning_root(&roots, "/ab/c.rs").unwrap(), "/ab");
        assert!(owning_root(&roots, "/x/c.rs").is_none());
    }

    #[gpui::test]
    fn the_current_project_leads_and_repeats_are_dropped(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            publish(&["/p/one".into(), "/p/two".into(), "~".into()], cx);
            assert_eq!(
                local_projects(cx, Some("/p/two")),
                ["/p/two".to_owned(), "/p/one".to_owned()]
            );
            assert_eq!(local_projects(cx, None), ["/p/one", "/p/two"]);
        });
    }
}
