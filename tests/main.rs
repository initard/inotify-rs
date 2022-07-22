#![deny(warnings)]

// This test suite is incomplete and doesn't cover all available functionality.
// Contributions to improve test coverage would be highly appreciated!

use futures_util::StreamExt;
use inotify::{EventMask, Inotify, WatchMask};
use maplit::hashmap;
use rand::prelude::*;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
use std::path::PathBuf;
use std::{
    fs::{self, File},
    sync::Arc,
};
use std::{
    io::{ErrorKind, Write},
    sync::Mutex,
};
use tempfile::TempDir;

#[test]
fn it_should_watch_a_file() {
    let mut testdir = TestDir::new();
    let (path, mut file) = testdir.new_file();

    let mut inotify = Inotify::init().unwrap();
    let watch = inotify.add_watch(&path, WatchMask::MODIFY).unwrap();

    write_to(&mut file);

    let mut buffer = [0; 1024];
    let events = inotify.read_events_blocking(&mut buffer).unwrap();

    let mut num_events = 0;
    for event in events {
        assert_eq!(watch, event.wd);
        num_events += 1;
    }
    assert!(num_events > 0);
}

#[cfg(feature = "stream")]
#[tokio::test]
async fn it_should_watch_a_file_async() {
    let mut testdir = TestDir::new();
    let (path, mut file) = testdir.new_file();

    let mut inotify = Inotify::init().unwrap();
    let watch = inotify.add_watch(&path, WatchMask::MODIFY).unwrap();

    write_to(&mut file);

    let mut buffer = [0; 1024];

    use futures_util::StreamExt;
    let events = inotify
        .event_stream(&mut buffer[..])
        .unwrap()
        .take(1)
        .collect::<Vec<_>>()
        .await;

    let mut num_events = 0;
    for event in events {
        if let Ok(event) = event {
            assert_eq!(watch, event.wd);
            num_events += 1;
        }
    }
    assert!(num_events > 0);
}

#[test]
fn it_should_return_immediately_if_no_events_are_available() {
    let mut inotify = Inotify::init().unwrap();

    let mut buffer = [0; 1024];
    assert_eq!(
        inotify.read_events(&mut buffer).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}

#[test]
fn it_should_convert_the_name_into_an_os_str() {
    let mut testdir = TestDir::new();
    let (path, mut file) = testdir.new_file();

    let mut inotify = Inotify::init().unwrap();
    inotify
        .add_watch(&path.parent().unwrap(), WatchMask::MODIFY)
        .unwrap();

    write_to(&mut file);

    let mut buffer = [0; 1024];
    let mut events = inotify.read_events_blocking(&mut buffer).unwrap();

    if let Some(event) = events.next() {
        assert_eq!(path.file_name(), event.name);
    } else {
        panic!("Expected inotify event");
    }
}

#[test]
fn it_should_set_name_to_none_if_it_is_empty() {
    let mut testdir = TestDir::new();
    let (path, mut file) = testdir.new_file();

    let mut inotify = Inotify::init().unwrap();
    inotify.add_watch(&path, WatchMask::MODIFY).unwrap();

    write_to(&mut file);

    let mut buffer = [0; 1024];
    let mut events = inotify.read_events_blocking(&mut buffer).unwrap();

    if let Some(event) = events.next() {
        assert_eq!(event.name, None);
    } else {
        panic!("Expected inotify event");
    }
}

#[test]
fn it_should_not_accept_watchdescriptors_from_other_instances() {
    let mut testdir = TestDir::new();
    let (path, _) = testdir.new_file();

    let mut inotify = Inotify::init().unwrap();
    let _ = inotify.add_watch(&path, WatchMask::ACCESS).unwrap();

    let mut second_inotify = Inotify::init().unwrap();
    let wd2 = second_inotify.add_watch(&path, WatchMask::ACCESS).unwrap();

    assert_eq!(
        inotify.rm_watch(wd2).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn watch_descriptors_from_different_inotify_instances_should_not_be_equal() {
    let mut testdir = TestDir::new();
    let (path, _) = testdir.new_file();

    let mut inotify_1 = Inotify::init().unwrap();
    let mut inotify_2 = Inotify::init().unwrap();

    let wd_1 = inotify_1.add_watch(&path, WatchMask::ACCESS).unwrap();
    let wd_2 = inotify_2.add_watch(&path, WatchMask::ACCESS).unwrap();

    // As far as inotify is concerned, watch descriptors are just integers that
    // are scoped per inotify instance. This means that multiple instances will
    // produce the same watch descriptor number, a case we want inotify-rs to
    // detect.
    assert!(wd_1 != wd_2);
}

#[test]
fn watch_descriptor_equality_should_not_be_confused_by_reused_fds() {
    let mut testdir = TestDir::new();
    let (path, _) = testdir.new_file();

    // When a new inotify instance is created directly after closing another
    // one, it is possible that the file descriptor is reused immediately, and
    // we end up with a new instance that has the same file descriptor as the
    // old one.
    // This is quite likely, but it doesn't happen every time. Therefore we may
    // need a few tries until we find two instances where that is the case.
    let (wd_1, mut inotify_2) = loop {
        let mut inotify_1 = Inotify::init().unwrap();

        let wd_1 = inotify_1.add_watch(&path, WatchMask::ACCESS).unwrap();
        let fd_1 = inotify_1.as_raw_fd();

        inotify_1.close().unwrap();
        let inotify_2 = Inotify::init().unwrap();

        if fd_1 == inotify_2.as_raw_fd() {
            break (wd_1, inotify_2);
        }
    };

    let wd_2 = inotify_2.add_watch(&path, WatchMask::ACCESS).unwrap();

    // The way we engineered this situation, both `WatchDescriptor` instances
    // have the same fields. They still come from different inotify instances
    // though, so they shouldn't be equal.
    assert!(wd_1 != wd_2);

    inotify_2.close().unwrap();

    // A little extra gotcha: If both inotify instances are closed, and the `Eq`
    // implementation naively compares the weak pointers, both will be `None`,
    // making them equal. Let's make sure this isn't the case.
    assert!(wd_1 != wd_2);
}

#[test]
fn it_should_implement_raw_fd_traits_correctly() {
    let fd = Inotify::init()
        .expect("Failed to initialize inotify instance")
        .into_raw_fd();

    // If `IntoRawFd` has been implemented naively, `Inotify`'s `Drop`
    // implementation will have closed the inotify instance at this point. Let's
    // make sure this didn't happen.
    let mut inotify = unsafe { <Inotify as FromRawFd>::from_raw_fd(fd) };

    let mut buffer = [0; 1024];
    if let Err(error) = inotify.read_events(&mut buffer) {
        if error.kind() != ErrorKind::WouldBlock {
            panic!("Failed to add watch: {}", error);
        }
    }
}

#[tokio::test]
/// Testing if two files with the same name but different directories
/// (e.g. "file_a" and "another_dir/file_a") are distinguished when _randomly_
/// triggering a DELETE_SELF for the two files.
async fn it_should_distinguish_event_for_files_with_same_name() {
    let mut testdir = TestDir::new();
    let testdir_path = testdir.dir.path().to_owned();
    let file_order = Arc::new(Mutex::new(vec!["file_a", "another_dir/file_a"]));
    file_order.lock().unwrap().shuffle(&mut thread_rng());
    let file_order_clone = file_order.clone();

    let mut inotify = Inotify::init().expect("Failed to initialize inotify instance");

    // creating file_a inside `TestDir.dir`
    let (path_1, _) = testdir.new_file_with_name("file_a");
    // creating a directory inside `TestDir.dir`
    testdir.new_directory_with_name("another_dir");
    // creating a file inside `TestDir.dir/another_dir`
    let (path_2, _) = testdir.new_file_in_directory_with_name("another_dir", "file_a");

    // watching both files for `DELETE_SELF`
    let wd_1 = inotify.add_watch(&path_1, WatchMask::DELETE_SELF).unwrap();
    let wd_2 = inotify.add_watch(&path_2, WatchMask::DELETE_SELF).unwrap();

    let expected_ids = hashmap! {
        wd_1.get_watch_descriptor_id() => "file_a",
        wd_2.get_watch_descriptor_id() => "another_dir/file_a"
    };
    let mut buffer = [0; 1024];

    let file_removal_handler = tokio::spawn(async move {
        for file in file_order.lock().unwrap().iter() {
            testdir.delete_file(file);
        }
    });

    let event_handle = tokio::spawn(async move {
        let mut events = inotify.event_stream(&mut buffer).unwrap();
        while let Some(Ok(event)) = events.next().await {
            if event.mask == EventMask::DELETE_SELF {
                let id = event.wd.get_watch_descriptor_id();
                let file = expected_ids.get(&id).unwrap();
                let full_path = testdir_path.join(*file);
                println!("file {:?} was deleted", full_path);
                file_order_clone.lock().unwrap().retain(|&x| x != *file);

                if file_order_clone.lock().unwrap().is_empty() {
                    break;
                }
            }
        }
    });

    let () = event_handle.await.unwrap();
    let () = file_removal_handler.await.unwrap();
}

struct TestDir {
    dir: TempDir,
    counter: u32,
}

impl TestDir {
    fn new() -> TestDir {
        TestDir {
            dir: TempDir::new().unwrap(),
            counter: 0,
        }
    }

    fn new_file_with_name(&mut self, file_name: &str) -> (PathBuf, File) {
        self.counter += 1;

        let path = self.dir.path().join(file_name);
        let file = File::create(&path)
            .unwrap_or_else(|error| panic!("Failed to create temporary file: {}", error));

        (path, file)
    }

    fn delete_file(&mut self, relative_path_to_file: &str) {
        let path = &self.dir.path().join(relative_path_to_file);
        fs::remove_file(path).unwrap();
    }

    fn new_file_in_directory_with_name(
        &mut self,
        dir_name: &str,
        file_name: &str,
    ) -> (PathBuf, File) {
        self.counter += 1;

        let path = self.dir.path().join(dir_name).join(file_name);
        let file = File::create(&path)
            .unwrap_or_else(|error| panic!("Failed to create temporary file: {}", error));

        (path, file)
    }

    fn new_directory_with_name(&mut self, dir_name: &str) -> PathBuf {
        let path = self.dir.path().join(dir_name);
        let () = fs::create_dir(&path).unwrap();
        path.to_path_buf()
    }

    fn new_file(&mut self) -> (PathBuf, File) {
        let id = self.counter;
        self.counter += 1;

        let path = self.dir.path().join("file-".to_string() + &id.to_string());
        let file = File::create(&path)
            .unwrap_or_else(|error| panic!("Failed to create temporary file: {}", error));

        (path, file)
    }
}

fn write_to(file: &mut File) {
    file.write(b"This should trigger an inotify event.")
        .unwrap_or_else(|error| panic!("Failed to write to file: {}", error));
}
