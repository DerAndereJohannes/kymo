//! Files handed over by the system: the inbox, and on macOS the method added to the app
//! delegate that fills it.

use std::path::PathBuf;
use std::sync::Arc;

use kymo::opened::Inbox;

#[test]
fn inbox_keeps_paths_until_taken() {
    let inbox = Inbox::new();
    assert!(inbox.is_empty());
    inbox.push([PathBuf::from("a.edf")]);
    inbox.push([PathBuf::from("b.bdf"), PathBuf::from("c.fif")]);
    assert!(!inbox.is_empty());
    assert_eq!(inbox.take(), ["a.edf", "b.bdf", "c.fif"].map(PathBuf::from));
    assert!(inbox.is_empty());
    assert!(inbox.take().is_empty());
}

/// `application:openURLs:`, added to a class of the test's own (winit's delegate needs an
/// event loop on the main thread), passes on the paths of file URLs only.
#[cfg(target_os = "macos")]
#[test]
#[allow(unsafe_code)]
fn open_urls_puts_file_paths_in_the_inbox() {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{ClassType, define_class, msg_send};
    use objc2_foundation::{NSArray, NSString, NSURL};

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "KymoTestDelegate"]
        struct TestDelegate;
    );

    let inbox = Arc::new(Inbox::new());
    kymo::opened::receive_on(inbox.clone());
    assert!(kymo::opened::add_open_urls(TestDelegate::class()));
    // Added once; an existing method stays.
    assert!(!kymo::opened::add_open_urls(TestDelegate::class()));

    let file = NSURL::fileURLWithPath(&NSString::from_str("/tmp/kymo test/a.edf"));
    let web = NSURL::URLWithString(&NSString::from_str("https://example.com/b.bdf")).unwrap();
    let urls = NSArray::from_retained_slice(&[file, web]);
    // SAFETY: a fresh object of a plain NSObject subclass; the method was added above with
    // the signature used here.
    unsafe {
        let delegate: Retained<TestDelegate> = msg_send![TestDelegate::class(), new];
        let _: () = msg_send![
            &*delegate,
            application: std::ptr::null::<AnyObject>(),
            openURLs: &*urls
        ];
    }
    assert_eq!(inbox.take(), [PathBuf::from("/tmp/kymo test/a.edf")]);
}
