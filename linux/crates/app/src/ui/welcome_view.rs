use relm4::adw::prelude::*;
use relm4::factory::FactoryVecDeque;
use relm4::prelude::*;
use relm4::{adw, gtk};

use tablepro_storage::{ConnectionOrganizationIndex, SavedConnection, arrange_connections};
use uuid::Uuid;

use super::connection_row::{ConnectionRow, ConnectionRowInit, ConnectionRowOutput};

pub struct WelcomeView {
    connections: Vec<SavedConnection>,
    organization: ConnectionOrganizationIndex,
    filter: String,
    factory: FactoryVecDeque<ConnectionRow>,
    stack: gtk::Stack,
    search: gtk::SearchEntry,
    empty_filter_page: adw::StatusPage,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum WelcomeViewInput {
    SetConnections(Vec<SavedConnection>),
    SetOrganization(ConnectionOrganizationIndex),
    FilterChanged(String),
    OpenConnect,
    ImportUrl,
    OpenSaved(SavedConnection),
    ToggleFavorite(Uuid),
    Organize(SavedConnection),
    Delete(Uuid),
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum WelcomeViewOutput {
    OpenConnect,
    ImportUrl,
    OpenSaved(SavedConnection),
    ToggleFavorite(Uuid),
    Organize(SavedConnection),
    Delete(Uuid),
}

#[derive(Debug, Default)]
pub struct WelcomeViewInit;

impl SimpleComponent for WelcomeView {
    type Init = WelcomeViewInit;
    type Input = WelcomeViewInput;
    type Output = WelcomeViewOutput;
    type Root = gtk::Stack;
    type Widgets = ();

    fn init_root() -> Self::Root {
        gtk::Stack::builder().build()
    }

    fn init(_init: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let factory: FactoryVecDeque<ConnectionRow> = FactoryVecDeque::builder()
            .launch(
                gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::None)
                    .css_classes(["boxed-list"])
                    .build(),
            )
            .forward(sender.input_sender(), |out| match out {
                ConnectionRowOutput::Open(saved) => WelcomeViewInput::OpenSaved(saved),
                ConnectionRowOutput::ToggleFavorite(id) => WelcomeViewInput::ToggleFavorite(id),
                ConnectionRowOutput::Organize(saved) => WelcomeViewInput::Organize(saved),
                ConnectionRowOutput::Delete(id) => WelcomeViewInput::Delete(id),
            });

        // Empty page — no saved connections yet. GNOME convention is
        // state / instruction / action — title states the situation,
        // description tells the user what to do, the button restates
        // the action with verb-first phrasing (matches Settings's
        // "No printers found" / "Add a printer to begin." / "Add
        // Printer" pattern).
        let empty_page = adw::StatusPage::builder()
            .icon_name("network-server-symbolic")
            .title(crate::tr!("No connections yet"))
            .description(crate::tr!("Add a database connection to get started."))
            .build();
        let empty_btn = gtk::Button::builder()
            .label(crate::tr!("Add Connection"))
            .halign(gtk::Align::Center)
            .build();
        empty_btn.add_css_class("suggested-action");
        empty_btn.add_css_class("pill");
        let s_empty = sender.clone();
        empty_btn.connect_clicked(move |_| s_empty.input(WelcomeViewInput::OpenConnect));
        empty_page.set_child(Some(&empty_btn));
        root.add_named(&empty_page, Some("empty"));

        // Populated page — saved connections list. AdwClamp is the
        // GNOME pattern for "constrain reading width to a sensible
        // max in a scrollable area"; it centres + caps width without
        // the manual `gtk::Box` halign/margin gymnastics.
        let scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let clamp = adw::Clamp::builder().maximum_size(560).build();
        let outer = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();

        // Single CTA on the populated page: the "+" button in the
        // group header. Previously we also rendered a bottom pill
        // labelled "New connection", which duplicated the affordance —
        // ambiguity at different visual weights. Empty-page pill
        // stays (it's the only CTA there); on this page the header
        // suffix is sufficient.
        let group = adw::PreferencesGroup::builder()
            .title(crate::tr!("Saved connections"))
            .build();
        let header_btn = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(crate::tr!("Add Connection"))
            .valign(gtk::Align::Center)
            .build();
        header_btn.add_css_class("flat");
        let s_header = sender.clone();
        header_btn.connect_clicked(move |_| s_header.input(WelcomeViewInput::OpenConnect));
        // Import sits beside Add rather than inside a menu: pasting a
        // connection URL is the fastest path in from another tool, and
        // GNOME puts sibling create-actions side by side in the group
        // header (Software's "Add" / "Install File…" pattern).
        let import_btn = gtk::Button::builder()
            .icon_name("insert-link-symbolic")
            .tooltip_text(crate::tr!("Import from URL"))
            .valign(gtk::Align::Center)
            .build();
        import_btn.add_css_class("flat");
        let s_import = sender.clone();
        import_btn.connect_clicked(move |_| s_import.input(WelcomeViewInput::ImportUrl));
        let header_actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        header_actions.append(&import_btn);
        header_actions.append(&header_btn);
        group.set_header_suffix(Some(&header_actions));

        // Filter box above the list. Hidden until there are enough
        // connections to be worth filtering — a search field over three
        // rows is noise, and GNOME only reveals search once a list can
        // outgrow the viewport.
        let search = gtk::SearchEntry::builder()
            .placeholder_text(crate::tr!("Filter by name, group, tag or driver"))
            .hexpand(true)
            .visible(false)
            .build();
        let s_search = sender.clone();
        search.connect_search_changed(move |entry| {
            s_search.input(WelcomeViewInput::FilterChanged(entry.text().to_string()));
        });
        outer.append(&search);
        group.add(factory.widget());
        outer.append(&group);

        // Shown in place of the list when every connection is filtered
        // out. Distinct from the "no connections yet" page: the fix is
        // to clear the filter, not to add a connection.
        let empty_filter_page = adw::StatusPage::builder()
            .icon_name("system-search-symbolic")
            .title(crate::tr!("No matches"))
            .description(crate::tr!("No saved connection matches this filter."))
            .visible(false)
            .build();
        empty_filter_page.add_css_class("compact");
        outer.append(&empty_filter_page);
        clamp.set_child(Some(&outer));

        scroller.set_child(Some(&clamp));
        root.add_named(&scroller, Some("populated"));
        root.set_visible_child_name("empty");

        let model = WelcomeView {
            connections: Vec::new(),
            organization: ConnectionOrganizationIndex::default(),
            filter: String::new(),
            factory,
            stack: root.clone(),
            search,
            empty_filter_page,
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            WelcomeViewInput::SetConnections(connections) => {
                self.connections = connections;
                self.rebuild_rows();
            }
            WelcomeViewInput::SetOrganization(organization) => {
                self.organization = organization;
                self.rebuild_rows();
            }
            WelcomeViewInput::FilterChanged(filter) => {
                self.filter = filter;
                self.rebuild_rows();
            }
            WelcomeViewInput::OpenConnect => {
                let _ = sender.output(WelcomeViewOutput::OpenConnect);
            }
            WelcomeViewInput::ImportUrl => {
                let _ = sender.output(WelcomeViewOutput::ImportUrl);
            }
            WelcomeViewInput::OpenSaved(saved) => {
                let _ = sender.output(WelcomeViewOutput::OpenSaved(saved));
            }
            WelcomeViewInput::ToggleFavorite(id) => {
                let _ = sender.output(WelcomeViewOutput::ToggleFavorite(id));
            }
            WelcomeViewInput::Organize(saved) => {
                let _ = sender.output(WelcomeViewOutput::Organize(saved));
            }
            WelcomeViewInput::Delete(id) => {
                let _ = sender.output(WelcomeViewOutput::Delete(id));
            }
        }
    }
}

/// Connections a filter cannot outgrow. Below this the filter box stays
/// hidden, so the field only appears once scanning the list by eye stops
/// being the faster option.
const FILTER_REVEAL_THRESHOLD: usize = 6;

impl WelcomeView {
    fn rebuild_rows(&mut self) {
        // Favourites first, then group, then name — the ordering lives
        // in tablepro-storage so the same arrangement is testable
        // without a GTK main context. Recency is a deliberate casualty:
        // an explicit favourite outranks "whatever I opened last".
        let arranged = arrange_connections(&self.connections, &self.organization, &self.filter);
        let mut guard = self.factory.guard();
        guard.clear();
        for saved in &arranged {
            let organization = self.organization.get(saved.id);
            guard.push_back(ConnectionRowInit {
                saved: saved.clone(),
                organization,
            });
        }
        drop(guard);

        self.search
            .set_visible(self.connections.len() >= FILTER_REVEAL_THRESHOLD);
        self.empty_filter_page
            .set_visible(arranged.is_empty() && !self.connections.is_empty());
        let name = if self.connections.is_empty() {
            "empty"
        } else {
            "populated"
        };
        self.stack.set_visible_child_name(name);
    }
}
