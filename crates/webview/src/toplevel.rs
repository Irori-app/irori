//! A GTK window that renders offscreen like `GtkOffscreenWindow`, without being one.
//!
//! WebKitGTK only puts a page "in a window" when its toplevel is a `GtkWindow` that is not a
//! `GtkOffscreenWindow`. Out of a window, WebKit holds back media and image animation, so the
//! page is hosted here instead: a plain `GtkWindow` subclass that realizes an offscreen GDK
//! window, mirroring GTK 3's `gtkoffscreenwindow.c`.

use std::cell::RefCell;

use gtk::glib;
use gtk::glib::translate::{FromGlibPtrNone as _, ToGlibPtr as _};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{cairo, gdk};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct OffscreenToplevel {
        /// A toplevel that is never shown, through which GDK resolves the offscreen window's
        /// monitor; without one GDK looks up a missing window and logs criticals.
        pub embedder: RefCell<Option<gdk::Window>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OffscreenToplevel {
        const NAME: &'static str = "GpuiWebViewOffscreenToplevel";
        type Type = super::OffscreenToplevel;
        type ParentType = gtk::Window;

        fn class_init(klass: &mut Self::Class) {
            // gtk-rs offers no show/hide overrides, so set the class functions directly.
            let widget_class = klass as *mut Self::Class as *mut gtk::ffi::GtkWidgetClass;
            // SAFETY: The class struct starts with its GtkWidgetClass.
            unsafe {
                (*widget_class).show = Some(show);
                (*widget_class).hide = Some(hide);
            }
        }
    }

    /// The class functions of GtkWidget itself, skipping GtkWindow's.
    fn widget_base_class() -> *const gtk::ffi::GtkWidgetClass {
        // SAFETY: GtkWidget's class exists once GTK is initialized.
        unsafe {
            glib::gobject_ffi::g_type_class_peek(gtk::ffi::gtk_widget_get_type())
                as *const gtk::ffi::GtkWidgetClass
        }
    }

    unsafe extern "C" fn show(widget: *mut gtk::ffi::GtkWidget) {
        // SAFETY: GTK calls this with a live widget of this class. GtkWidget's own show sets
        // the visible flag; GtkWindow's would put a window on screen.
        unsafe {
            if let Some(show) = (*widget_base_class()).show {
                show(widget);
            }
            let Ok(window) =
                gtk::Widget::from_glib_none(widget).downcast::<super::OffscreenToplevel>()
            else {
                return;
            };
            resize(&window);
            window.map();
            if window.focused_widget().is_none() {
                window.child_focus(gtk::DirectionType::TabForward);
            }
        }
    }

    unsafe extern "C" fn hide(widget: *mut gtk::ffi::GtkWidget) {
        // SAFETY: As in `show`; GtkWidget's hide clears the visible flag and unmaps.
        unsafe {
            if let Some(hide) = (*widget_base_class()).hide {
                hide(widget);
            }
        }
    }

    impl ObjectImpl for OffscreenToplevel {}

    impl WidgetImpl for OffscreenToplevel {
        fn realize(&self) {
            let obj = self.obj();
            let widget: *mut gtk::ffi::GtkWidget = obj.upcast_ref::<gtk::Widget>().to_glib_none().0;
            // SAFETY: The calls mirror `gtk_offscreen_window_realize` on this live widget; the
            // widget takes the new window's reference.
            unsafe {
                gtk::ffi::gtk_widget_set_realized(widget, glib::ffi::GTRUE);
                let mut allocation = std::mem::zeroed();
                gtk::ffi::gtk_widget_get_allocation(widget, &mut allocation);
                let mut attributes: gdk::ffi::GdkWindowAttr = std::mem::zeroed();
                attributes.x = allocation.x;
                attributes.y = allocation.y;
                attributes.width = allocation.width;
                attributes.height = allocation.height;
                attributes.window_type = gdk::ffi::GDK_WINDOW_OFFSCREEN;
                attributes.event_mask =
                    gtk::ffi::gtk_widget_get_events(widget) | gdk::ffi::GDK_EXPOSURE_MASK as i32;
                attributes.visual = gtk::ffi::gtk_widget_get_visual(widget);
                attributes.wclass = gdk::ffi::GDK_INPUT_OUTPUT;
                let mask = gdk::ffi::GDK_WA_X | gdk::ffi::GDK_WA_Y | gdk::ffi::GDK_WA_VISUAL;
                let window =
                    gdk::ffi::gdk_window_new(std::ptr::null_mut(), &mut attributes, mask as i32);
                gtk::ffi::gtk_widget_set_window(widget, window);
                gtk::ffi::gtk_widget_register_window(widget, window);
                if let Some(child) = obj.child() {
                    gtk::ffi::gtk_widget_set_parent_window(child.to_glib_none().0, window);
                }
                let embedder = gdk::Window::new(
                    None,
                    &gdk::WindowAttr {
                        window_type: gdk::WindowType::Toplevel,
                        width: 1,
                        height: 1,
                        wclass: gdk::WindowWindowClass::InputOutput,
                        ..Default::default()
                    },
                );
                gdk::ffi::gdk_offscreen_window_set_embedder(window, embedder.to_glib_none().0);
                *self.embedder.borrow_mut() = Some(embedder);
            }
        }

        fn unrealize(&self) {
            self.parent_unrealize();
            if let Some(embedder) = self.embedder.take() {
                embedder.destroy();
            }
        }

        fn preferred_width(&self) -> (i32, i32) {
            let obj = self.obj();
            let (default, _) = obj.default_size();
            preferred(&obj, default, |child| child.preferred_width())
        }

        fn preferred_height(&self) -> (i32, i32) {
            let obj = self.obj();
            let (_, default) = obj.default_size();
            preferred(&obj, default, |child| child.preferred_height())
        }

        fn size_allocate(&self, allocation: &gtk::Allocation) {
            let obj = self.obj();
            obj.set_allocation(allocation);
            if obj.is_realized()
                && let Some(window) = obj.window()
            {
                window.move_resize(
                    allocation.x(),
                    allocation.y(),
                    allocation.width(),
                    allocation.height(),
                );
            }
            let border = obj.border_width() as i32;
            if let Some(child) = obj.child().filter(|c| c.is_visible()) {
                child.size_allocate(&gtk::Allocation::new(
                    border,
                    border,
                    allocation.width() - 2 * border,
                    allocation.height() - 2 * border,
                ));
            }
            obj.queue_draw();
        }
    }

    impl ContainerImpl for OffscreenToplevel {
        fn check_resize(&self) {
            let obj = self.obj();
            if obj.is_visible() {
                resize(&obj);
            }
        }
    }

    impl BinImpl for OffscreenToplevel {}
    impl WindowImpl for OffscreenToplevel {}

    /// The child's size plus the border, at least the window's default size.
    fn preferred(
        window: &super::OffscreenToplevel,
        default: i32,
        child_size: impl Fn(&gtk::Widget) -> (i32, i32),
    ) -> (i32, i32) {
        let border = window.border_width() as i32 * 2;
        let (child_min, child_nat) = window
            .child()
            .filter(|c| c.is_visible())
            .map_or((0, 0), |child| child_size(&child));
        (
            (border + child_min).max(default),
            (border + child_nat).max(default),
        )
    }

    fn resize(window: &super::OffscreenToplevel) {
        let (requisition, _) = window.preferred_size();
        window.size_allocate(&gtk::Allocation::new(
            0,
            0,
            requisition.width,
            requisition.height,
        ));
    }
}

glib::wrapper! {
    pub struct OffscreenToplevel(ObjectSubclass<imp::OffscreenToplevel>)
        @extends gtk::Window, gtk::Bin, gtk::Container, gtk::Widget,
        @implements gtk::Buildable;
}

impl OffscreenToplevel {
    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    /// The offscreen surface holding the window's latest rendering.
    pub(crate) fn surface(&self) -> Option<cairo::Surface> {
        let window = self.window()?;
        // SAFETY: The window is this widget's offscreen GDK window; the surface is borrowed.
        unsafe {
            let surface = gdk::ffi::gdk_offscreen_window_get_surface(window.to_glib_none().0);
            (!surface.is_null()).then(|| cairo::Surface::from_glib_none(surface))
        }
    }
}
