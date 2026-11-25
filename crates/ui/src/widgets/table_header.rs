use std::{cell::RefCell, rc::Rc};

use gtk::glib;
use gtk::prelude::*;

use rufin_core::settings::LibraryListKey;
use rufin_core::settings::layout::LibraryField;

mod header_cell_imp {
    use std::{cell::RefCell, rc::Rc};

    use gtk::prelude::*;
    use gtk::{CompositeTemplate, TemplateChild, glib, subclass::prelude::*};

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/app/rufin/rufin/ui/routes/column_view_header_cell.ui")]
    pub struct HeaderCell {
        #[template_child]
        pub content: TemplateChild<gtk::Box>,
        #[template_child]
        pub label: TemplateChild<gtk::Label>,
        #[template_child]
        pub icon: TemplateChild<gtk::Image>,
        /// Sorts the list by this column; none when the column does not sort it.
        pub sort: RefCell<Option<Rc<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HeaderCell {
        const NAME: &'static str = "RufinColumnViewHeaderCell";
        type Type = super::HeaderCell;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for HeaderCell {
        fn constructed(&self) {
            self.parent_constructed();
            let click = gtk::GestureClick::new();
            let cell = self.obj().downgrade();
            click.connect_released(move |click, _, _, _| {
                let Some(cell) = cell.upgrade() else {
                    return;
                };
                let sort = cell.imp().sort.borrow().clone();
                if let Some(sort) = sort {
                    click.set_state(gtk::EventSequenceState::Claimed);
                    sort();
                }
            });
            self.obj().add_controller(click);
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            self.dispose_template();
        }
    }

    impl WidgetImpl for HeaderCell {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Horizontal {
                // The column's fixed width owns horizontal sizing, including clipped labels.
                (0, 0, -1, -1)
            } else {
                self.content.measure(orientation, for_size)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.content.allocate(width, height, baseline, None);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().snapshot_child(&*self.content, snapshot);
        }
    }
}

glib::wrapper! {
    pub struct HeaderCell(ObjectSubclass<header_cell_imp::HeaderCell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HeaderCell {
    pub fn new(title: &str, field: Option<LibraryField>, position: usize) -> Self {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let cell: Self = glib::Object::new();
        let imp = cell.imp();
        cell.update_property(&[gtk::accessible::Property::Label(title)]);
        cell.update_relation(&[gtk::accessible::Relation::ColIndex(position as i32 + 1)]);
        imp.label.set_label(title);
        imp.icon.set_tooltip_text(Some(title));
        imp.label
            .set_xalign(if field == Some(LibraryField::RowIndex) {
                0.5
            } else {
                0.0
            });
        let icon = match field {
            Some(LibraryField::Duration) => Some("rufin-preferences-system-time-symbolic"),
            Some(LibraryField::Favorite) => Some(crate::favorites::FAVORITE_ADD_ICON),
            _ => None,
        };
        imp.label.set_visible(icon.is_none());
        imp.icon
            .set_halign(if field == Some(LibraryField::Favorite) {
                gtk::Align::Center
            } else {
                gtk::Align::Start
            });
        imp.icon.set_icon_name(icon);
        imp.icon.set_visible(icon.is_some());
        cell
    }

    /// Makes a click sort the list by this column with `sort`, and tells assistive technologies
    /// the order when the list is sorted by it: `sorted` is whether that order is descending.
    pub fn set_sort(&self, sort: Option<Rc<dyn Fn()>>, sorted: Option<bool>) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let imp = self.imp();
        let sortable = sort.is_some();
        imp.sort.replace(sort);
        if sortable {
            self.add_css_class("sortable");
            self.set_cursor_from_name(Some("pointer"));
        } else {
            self.remove_css_class("sortable");
            self.set_cursor(None);
        }
        self.update_property(&[gtk::accessible::Property::Sort(match sorted {
            Some(true) => gtk::AccessibleSort::Descending,
            Some(false) => gtk::AccessibleSort::Ascending,
            None => gtk::AccessibleSort::None,
        })]);
    }
}

/// How a table header sorts its list: the list, what a click on a column does, and the list's
/// current sort and order.
#[derive(Clone)]
pub struct HeaderSort {
    pub key: LibraryListKey,
    pub sort_by: Rc<dyn Fn(LibraryField)>,
    pub current: (LibraryField, bool),
}

impl HeaderSort {
    /// Lets the header cell of `field`'s column sort the list, when that column sorts it.
    pub fn apply(&self, cell: &HeaderCell, field: Option<LibraryField>) {
        let sort = field.and_then(|field| {
            rufin_core::settings::library_column_sort(self.key, field).map(|sort| (field, sort))
        });
        let sorted = sort
            .filter(|(_, sort)| *sort == self.current.0)
            .map(|_| self.current.1);
        let action = sort.map(|(field, _)| {
            let sort_by = Rc::clone(&self.sort_by);
            Rc::new(move || sort_by(field)) as Rc<dyn Fn()>
        });
        cell.set_sort(action, sorted);
    }
}

#[derive(Clone)]
pub struct ColumnViewHeader {
    /// Clips the header like the table's scroller, so the column widths never set a minimum width.
    clip: gtk::ScrolledWindow,
    root: gtk::Box,
    bindings: Rc<RefCell<Vec<glib::Binding>>>,
    cells: Rc<RefCell<Vec<(HeaderCell, Option<LibraryField>)>>>,
    sort: Rc<RefCell<Option<HeaderSort>>>,
}

impl Default for ColumnViewHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl ColumnViewHeader {
    pub fn new() -> Self {
        let resource = "routes/column_view_header.ui";
        let builder = crate::ui_resource::builder(resource);
        let clip = crate::ui_resource::object(&builder, resource, "clip");
        let root = crate::ui_resource::object(&builder, resource, "header");
        crate::widgets::layout::configure_fill_width_clip(&clip, gtk::PolicyType::Never);
        Self {
            clip,
            root,
            bindings: Rc::default(),
            cells: Rc::default(),
            sort: Rc::default(),
        }
    }

    /// Lets the column headers sort the list, or not with none.
    pub fn set_sort(&self, sort: Option<HeaderSort>) {
        if let Some(sort) = &sort {
            for (cell, field) in self.cells.borrow().iter() {
                sort.apply(cell, *field);
            }
        } else {
            for (cell, _) in self.cells.borrow().iter() {
                cell.set_sort(None, None);
            }
        }
        self.sort.replace(sort);
    }

    pub fn widget(&self) -> gtk::Widget {
        self.clip.clone().upcast()
    }

    pub fn replace(&self, columns: &[(gtk::ColumnViewColumn, Option<LibraryField>)]) {
        for binding in self.bindings.take() {
            binding.unbind();
        }
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        let mut bindings = Vec::with_capacity(columns.len() * 2);
        let mut cells = Vec::with_capacity(columns.len());
        let sort = self.sort.borrow().clone();
        for (position, (column, field)) in columns.iter().enumerate() {
            let title = column.title().unwrap_or_default();
            let cell = HeaderCell::new(&title, *field, position);
            if let Some(sort) = &sort {
                sort.apply(&cell, *field);
            }
            bindings.push(
                column
                    .bind_property("fixed-width", &cell, "width-request")
                    .sync_create()
                    .build(),
            );
            bindings.push(
                column
                    .bind_property("visible", &cell, "visible")
                    .sync_create()
                    .build(),
            );
            self.root.append(&cell);
            cells.push((cell, *field));
        }
        self.bindings.replace(bindings);
        self.cells.replace(cells);
    }

    pub fn set_visible(&self, visible: bool) {
        self.clip.set_visible(visible);
    }
}

#[derive(Clone)]
pub struct TableFrame {
    root: gtk::Box,
    header_host: gtk::Box,
}

impl TableFrame {
    pub fn new(content: &impl IsA<gtk::Widget>, header: Option<&gtk::Widget>) -> Self {
        let resource = "routes/table_frame.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            root: gtk::Box,
            header_host: gtk::Box,
            content_host: gtk::Box,
        });
        content_host.append(content);
        let frame = Self { root, header_host };
        frame.set_header(header);
        frame
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn set_header(&self, header: Option<&gtk::Widget>) {
        while let Some(child) = self.header_host.first_child() {
            self.header_host.remove(&child);
        }
        if let Some(header) = header {
            self.header_host.append(header);
        }
        self.header_host.set_visible(header.is_some());
    }
}
