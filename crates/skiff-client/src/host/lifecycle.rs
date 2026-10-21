//! M1g lifecycle and group boundary. Exhaustive mappings, as in inventory:
//! a new core field or variant is a compile error until projected here.
use skiff_core::{group as core_group, session as core};
use skiff_host_api::{groups as api_group, lifecycle as api, Error};

use super::inventory::{project_role, restore_role};

/// The desktop's spec, as the Host contract carries it. `coordinator` is never
/// on the legacy wire (`serde(skip)`); a spec that sets it is refused here
/// rather than silently changed.
pub(super) fn project_spec(value: core::SessionSpec) -> Result<api::SessionSpec, Error> {
    let core::SessionSpec {
        label,
        name,
        role,
        cwd,
        command,
        args,
        peek,
        coordinator,
        cols,
        rows,
    } = value;
    if coordinator {
        return Err(Error::MalformedContract {
            message: "a coordinator session starts only through the coordinator request".into(),
        });
    }
    Ok(api::SessionSpec {
        label,
        name,
        role: project_role(role),
        cwd: cwd
            .map(|p| {
                p.into_os_string()
                    .into_string()
                    .map_err(|_| Error::MalformedContract {
                        message: "non-UTF-8 path in session spec".into(),
                    })
            })
            .transpose()?,
        command,
        args,
        peek,
        cols,
        rows,
    })
}

pub(super) fn restore_spec(value: api::SessionSpec) -> core::SessionSpec {
    let api::SessionSpec {
        label,
        name,
        role,
        cwd,
        command,
        args,
        peek,
        cols,
        rows,
    } = value;
    core::SessionSpec {
        label,
        name,
        role: restore_role(role),
        cwd: cwd.map(Into::into),
        command,
        args,
        peek,
        coordinator: false,
        cols,
        rows,
    }
}

pub(super) fn project_group(value: core_group::Group) -> api_group::Group {
    let core_group::Group {
        id,
        name,
        layout,
        focus,
        cwd,
    } = value;
    api_group::Group {
        id,
        name,
        layout: project_layout(layout),
        focus,
        cwd,
    }
}

pub(super) fn restore_group(value: api_group::Group) -> core_group::Group {
    let api_group::Group {
        id,
        name,
        layout,
        focus,
        cwd,
    } = value;
    core_group::Group {
        id,
        name,
        layout: restore_layout(layout),
        focus,
        cwd,
    }
}

fn project_layout(value: core_group::Layout) -> api_group::Layout {
    match value {
        core_group::Layout::Pane { session } => api_group::Layout::Pane { session },
        core_group::Layout::Split { dir, ratio, a, b } => api_group::Layout::Split {
            dir: match dir {
                core_group::SplitDir::Row => api_group::SplitDir::Row,
                core_group::SplitDir::Col => api_group::SplitDir::Col,
            },
            ratio,
            a: Box::new(project_layout(*a)),
            b: Box::new(project_layout(*b)),
        },
    }
}

fn restore_layout(value: api_group::Layout) -> core_group::Layout {
    match value {
        api_group::Layout::Pane { session } => core_group::Layout::Pane { session },
        api_group::Layout::Split { dir, ratio, a, b } => core_group::Layout::Split {
            dir: match dir {
                api_group::SplitDir::Row => core_group::SplitDir::Row,
                api_group::SplitDir::Col => core_group::SplitDir::Col,
            },
            ratio,
            a: Box::new(restore_layout(*a)),
            b: Box::new(restore_layout(*b)),
        },
    }
}
