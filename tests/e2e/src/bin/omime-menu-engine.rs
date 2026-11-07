//! Engine menus for tray regressions: duplicate submenu IDs and an oversized list.
use omime_sdk::{Context, Engine, Host, KeyEvent, Property, PropertyKind};

enum Menu {
    DuplicateSubmenus(usize),
    Large,
}

impl Engine for Menu {
    fn registered(&mut self, host: &Host) {
        let properties = match self {
            Self::DuplicateSubmenus(fanout) => {
                let submenu = |parent: &str| Property {
                    id: "loop".into(),
                    parent: parent.into(),
                    label: "MenuLoop".into(),
                    kind: PropertyKind::Menu,
                    ..Property::default()
                };
                let mut properties = vec![
                    Property {
                        id: "sentinel".into(),
                        label: "MenuSentinel".into(),
                        ..Property::default()
                    },
                    submenu(""),
                ];
                properties.extend((0..*fanout).map(|_| submenu("loop")));
                properties
            }
            Self::Large => (0..300)
                .map(|index| Property {
                    id: format!("entry-{index}"),
                    label: format!("Entry {index}"),
                    ..Property::default()
                })
                .collect(),
        };
        host.set_properties(&properties);
    }

    fn process_key(&mut self, _ctx: &mut Context<'_>, _key: &KeyEvent) -> bool {
        false
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let menu = match args.next().as_deref() {
        Some("duplicate-submenus") => Menu::DuplicateSubmenus(
            args.next()
                .ok_or_else(|| anyhow::anyhow!("missing submenu fan-out"))?
                .parse()?,
        ),
        Some("large") => Menu::Large,
        _ => anyhow::bail!("usage: omime-menu-engine duplicate-submenus <fan-out> | large"),
    };
    omime_sdk::run(menu)?;
    Ok(())
}
