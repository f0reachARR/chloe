//! ワークスペースを作るフォームと、ディスクと VM の大きさを変えるフォーム。

use chloe_controller::workspace::{DiskSizes, IdleStop, MachineSize, RootMode};

use super::form::{Field, Form};
use crate::ops::{NewWorkspace, WorkspaceInfo, format_mib};

/// 新しいワークスペースのフォーム。
pub fn new_form() -> Form {
    Form::new(
        "New workspace",
        vec![
            Field::text("name", "Name", "").help(
                "name for a personal workspace, org/name or org/team/name for a shared one.",
            ),
            Field::choice(
                "root",
                "Root",
                &["(settings default)", "ephemeral", "persistent"],
                "(settings default)",
            )
            .help("persistent keeps changes to the root file system (apt install) across restarts."),
            Field::text("workspace", "Workspace disk", "")
                .help("in GiB, like 50G. Empty for the default. Holds /workspace and the home directory."),
            Field::text("docker", "Docker disk", "")
                .help("in GiB, like 80G. Empty for the default. Holds Docker images and containers."),
            Field::text("cpus", "vCPUs", "")
                .help("the number of vCPUs (1-32). Empty for the default."),
            Field::text("memory", "Memory", "")
                .help("like 8G or 512M. Empty for the default."),
            Field::text("idle", "Idle stop", "")
                .help("stops the VM this long after the last connection closes, like 30m or 2h. never keeps it running. Empty for the default."),
        ],
    )
}

/// ディスクの大きさのフォーム。今の大きさで埋める。
pub fn disks_form(ws: &WorkspaceInfo) -> Form {
    let help = "in GiB. Disks only grow; the new size applies from the next start.";
    Form::new(
        format!("Disks of {}", ws.name),
        vec![
            Field::text(
                "workspace",
                "Workspace disk",
                &format!("{}G", ws.workspace_disk_gib),
            )
            .help(help),
            Field::text("docker", "Docker disk", &format!("{}G", ws.docker_disk_gib)).help(help),
        ],
    )
}

/// VM の大きさと停止までの時間のフォーム。今の値で埋める。
pub fn machine_form(ws: &WorkspaceInfo, idle: &str) -> Form {
    let next = "The new size applies from the next start.";
    Form::new(
        format!("Machine of {}", ws.name),
        vec![
            Field::text("cpus", "vCPUs", &ws.vcpus.to_string())
                .help(format!("the number of vCPUs (1-32). {next}")),
            Field::text("memory", "Memory", &format_mib(ws.memory_mib))
                .help(format!("like 8G or 512M. {next}")),
            Field::text("idle", "Idle stop", idle)
                .help("stops the VM this long after the last connection closes, like 30m or 2h. never keeps it running; default uses the gateway's."),
        ],
    )
}

/// 新しいワークスペースのフォームを、名前と作るときの指定にする。
pub fn to_new(form: &Form) -> Result<(String, NewWorkspace), String> {
    let name = form.text("name");
    if name.is_empty() {
        return Err("enter a name".into());
    }
    let root = match form.choice("root").as_str() {
        "ephemeral" => Some(RootMode::Ephemeral),
        "persistent" => Some(RootMode::Persistent),
        _ => None,
    };
    let options = NewWorkspace {
        root,
        disks: DiskSizes {
            workspace_gib: optional_gib(form, "workspace")?,
            docker_gib: optional_gib(form, "docker")?,
        },
        machine: MachineSize {
            vcpus: optional_vcpus(form)?,
            memory_mib: optional_mib(form)?,
        },
        idle_stop: optional_idle(form)?.unwrap_or_default(),
    };
    Ok((name, options))
}

/// ディスクのフォームを、変えた大きさにする。開いたときの大きさと同じ欄は `None`。
pub fn to_disks(form: &Form, workspace_gib: u32, docker_gib: u32) -> Result<DiskSizes, String> {
    // 変えなかった欄は送らない (既定のままのディスクを、今の既定の値に固定しない)。
    let changed = |key, now| Ok::<_, String>(optional_gib(form, key)?.filter(|gib| *gib != now));
    let disks = DiskSizes {
        workspace_gib: changed("workspace", workspace_gib)?,
        docker_gib: changed("docker", docker_gib)?,
    };
    if disks == DiskSizes::default() {
        return Err("change a size".into());
    }
    Ok(disks)
}

/// VM のフォームを、変えた大きさと停止までの時間にする。開いたときの値と同じ欄は `None`。
pub fn to_machine(
    form: &Form,
    vcpus: u32,
    memory_mib: u32,
    idle: &str,
) -> Result<(MachineSize, Option<IdleStop>), String> {
    // 変えなかった欄は送らない (既定のままの値を、今の既定の値に固定しない)。
    let machine = MachineSize {
        vcpus: optional_vcpus(form)?.filter(|n| *n != vcpus),
        memory_mib: optional_mib(form)?.filter(|n| *n != memory_mib),
    };
    let idle_stop = if form.text("idle") == idle {
        None
    } else {
        optional_idle(form)?
    };
    if machine == MachineSize::default() && idle_stop.is_none() {
        return Err("change a value".into());
    }
    Ok((machine, idle_stop))
}

/// ディスクの大きさの欄。空なら `None`。
fn optional_gib(form: &Form, key: &str) -> Result<Option<u32>, String> {
    let text = form.text(key);
    if text.is_empty() {
        return Ok(None);
    }
    crate::ops::parse_gib(&text)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// vCPU の数の欄。空なら `None`。
fn optional_vcpus(form: &Form) -> Result<Option<u32>, String> {
    let text = form.text("cpus");
    if text.is_empty() {
        return Ok(None);
    }
    text.parse()
        .map(Some)
        .map_err(|_| format!("invalid number of vCPUs: {text}"))
}

/// メモリの欄。空なら `None`。
fn optional_mib(form: &Form) -> Result<Option<u32>, String> {
    let text = form.text("memory");
    if text.is_empty() {
        return Ok(None);
    }
    crate::ops::parse_mib(&text)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// 停止までの時間の欄。空なら `None`。
fn optional_idle(form: &Form) -> Result<Option<IdleStop>, String> {
    let text = form.text("idle");
    if text.is_empty() {
        return Ok(None);
    }
    crate::ops::parse_idle_stop(&text)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::app::tests::{app, form_mut, screen};
    use super::super::app::{Action, Outcome};
    use super::super::form::tests::{ctrl, key, type_text};
    use super::*;
    use terminput::KeyCode;

    #[test]
    fn grows_disks_with_a_form() {
        let mut app = app();
        assert!(screen(&app).contains("20G/40G"));
        app.handle_key(key(KeyCode::Char('g')));
        // 今の大きさが入っている。変えた欄だけを送る。
        for _ in 0.."20G".len() {
            form_mut(&mut app).handle_key(key(KeyCode::Backspace));
        }
        type_text(form_mut(&mut app), "50G");
        let actions = app.handle_key(ctrl('s'));
        assert!(matches!(
            &actions[..],
            [Action::SetDisks { name, disks }] if name == "default"
                && *disks == DiskSizes { workspace_gib: Some(50), docker_gib: None }
        ));
        // 何も変えなければ送らない。
        app.apply(Outcome::Done {
            message: "ok".into(),
            reload: vec![],
        });
        app.handle_key(key(KeyCode::Char('g')));
        assert!(app.handle_key(ctrl('s')).is_empty());
        assert!(screen(&app).contains("change a size"));
    }

    #[test]
    fn changes_the_machine_with_a_form() {
        let mut app = app();
        assert!(screen(&app).contains("4 vCPU    4G  idle 30m"));
        app.handle_key(key(KeyCode::Char('m')));
        // 今の値が入っている。変えた欄だけを送る。
        form_mut(&mut app).handle_key(key(KeyCode::Backspace));
        type_text(form_mut(&mut app), "8");
        form_mut(&mut app).handle_key(key(KeyCode::Tab));
        form_mut(&mut app).handle_key(key(KeyCode::Tab));
        for _ in 0.."30m".len() {
            form_mut(&mut app).handle_key(key(KeyCode::Backspace));
        }
        type_text(form_mut(&mut app), "never");
        let actions = app.handle_key(ctrl('s'));
        assert!(matches!(
            &actions[..],
            [Action::SetMachine { name, machine, idle_stop: Some(IdleStop::Never) }]
                if name == "default"
                    && *machine == MachineSize { vcpus: Some(8), memory_mib: None }
        ));
        // 何も変えなければ送らない。
        app.apply(Outcome::Done {
            message: "ok".into(),
            reload: vec![],
        });
        app.handle_key(key(KeyCode::Char('m')));
        assert!(app.handle_key(ctrl('s')).is_empty());
        assert!(screen(&app).contains("change a value"));
    }

    #[test]
    fn creates_a_workspace_with_a_form() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('n')));
        type_text(form_mut(&mut app), "api");
        form_mut(&mut app).handle_key(key(KeyCode::Tab));
        form_mut(&mut app).handle_key(key(KeyCode::Right));
        let actions = app.handle_key(ctrl('s'));
        assert!(matches!(
            &actions[..],
            [Action::CreateWorkspace { name, options }]
                if name == "api" && *options == NewWorkspace {
                    root: Some(RootMode::Ephemeral),
                    ..Default::default()
                }
        ));
        // 失敗すると、フォームに理由を出して開いたままにする。
        app.apply(Outcome::Failed("already exists: api".into()));
        assert!(screen(&app).contains("already exists: api"));
        app.handle_key(ctrl('s'));
        app.apply(Outcome::Done {
            message: "created api".into(),
            reload: vec![Action::LoadWorkspaces],
        });
        assert!(app.modals.is_empty());
        assert!(screen(&app).contains("created api"));
    }
}
