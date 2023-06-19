use nix::{
    sys::wait::waitpid,
    unistd::{fork, ForkResult},
};
use std::thread::sleep;
use std::thread::spawn;
use std::time::Duration;

use shared_hashmap::{SharedMemoryHashMap};

#[test]
fn test_insert() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, 1);
    assert_eq!(map.get(&1), Some(1));

    assert_eq!(map.get(&2), None);
    map.insert(2, 2);
    assert_eq!(map.get(&2), Some(2));
}

#[test]
fn test_get() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, 1);
    assert_eq!(map.get(&1), Some(1));
}

#[test]
fn test_get_immutable() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert("Hello", "World".to_owned());
    let mut value = map.get(&"Hello").unwrap();
    value.push_str("!");
    assert_eq!(map.get(&"Hello"), Some("World".to_owned()));
}

#[test]
fn test_peak() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, 1);
    map.insert(2, 2);
    assert_eq!(map.peak(&1), Some(1));
}

#[test]
fn test_get_lru() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, 1);
    map.insert(2, 2);
    map.insert(3, 3);
    map.get(&1);
    map.get(&3);

    assert_eq!(map.get_lru(), Some((2, 2)));

    map.get(&2);
    assert_eq!(map.get_lru(), Some((1, 1)));
}

#[test]
fn test_lru_evicted() {
    let mut map = SharedMemoryHashMap::new(168).unwrap();
    map.try_insert(1, "Hello").unwrap();
    assert_eq!(map.get(&1), Some("Hello"));
    map.try_insert(2, "Hello World").unwrap();
    assert_eq!(map.get(&1), None);
    assert_eq!(map.get(&2), Some("Hello World"));

    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    for i in 0..10000 {
        map.try_insert(i, "Hello".repeat(10)).unwrap();
    }
}

#[test]
fn test_insert_slice() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, [1, 2, 3]);
    map.insert(2, [3, 4, 5]);
    assert_eq!(map.get(&1), Some([1, 2, 3]));
    assert_eq!(map.get(&2), Some([3, 4, 5]));
}

#[test]
fn test_insert_string() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, String::from("Hello"));
    map.insert(2, String::from("World"));
    assert_eq!(map.get(&1), Some(String::from("Hello")));
    assert_eq!(map.get(&2), Some(String::from("World")));
}

#[test]
fn test_insert_too_long() {
    //let mut map = SharedMemoryHashMap::new(128).unwrap();
}

#[test]
fn test_remove() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, 1);
    map.insert(2, 2);
    map.insert(3, 3);
    map.remove(&1);
    assert_eq!(map.get(&1), None);
    assert_eq!(map.get(&2), Some(2));
    assert_eq!(map.get(&3), Some(3));
    assert_eq!(map.len(), 2);

    map.remove(&3);
    assert_eq!(map.get(&1), None);
    assert_eq!(map.get(&2), Some(2));
    assert_eq!(map.get(&3), None);
    assert_eq!(map.len(), 1);

    map.remove(&2);
    assert_eq!(map.get(&1), None);
    assert_eq!(map.get(&2), None);
    assert_eq!(map.get(&3), None);
    assert_eq!(map.len(), 0);
}

#[test]
fn test_contains_key() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, 1);
    assert_eq!(map.contains_key(&1), true);
    assert_eq!(map.contains_key(&2), false);
}

#[test]
fn test_clear() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, 1);
    map.insert(2, 2);
    map.insert(3, 3);
    map.clear();
    assert_eq!(map.get(&1), None);
    assert_eq!(map.get(&2), None);
    assert_eq!(map.get(&3), None);
    assert_eq!(map.len(), 0);
}

#[test]
fn test_clone() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    let mut map2 = map.clone();
    map.insert(1, 1);
    assert_eq!(map2.get(&1), Some(1));

    map2.clear();
    assert_eq!(map.len(), 0);
    assert_eq!(map2.len(), 0);
}

#[test]
fn test_used() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, 1);
    let used = map.used();
    map.insert(2, 2);
    assert!(map.used() > used);
}

#[test]
fn test_free() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, 1);
    assert_eq!(map.free(), 1024 - map.used());
}

#[test]
fn test_race_condition() {
    let mut map = SharedMemoryHashMap::new(1024 * 1024).unwrap();
    map.insert(1, 1);
    map.insert(2, 2);
    map.insert(3, 3);

    let mut map2 = map.clone();
    let mut map3 = map.clone();

    spawn(move || {
        for i in 100..2000 {
            map2.try_insert(i, i).unwrap();
        }
    });
    spawn(move || {
        for i in 2000..3000 {
            map3.try_insert(i, i).unwrap();
        }
    });

    sleep(Duration::from_millis(100));
}

#[test]
fn test_fork() {
    let mut map = SharedMemoryHashMap::new(1024).unwrap();
    map.insert(1, 1);
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child, .. }) => {
            map.insert(2, 2);
            waitpid(child, None).unwrap();
            assert_eq!(map.get(&3), Some(3));
        }
        Ok(ForkResult::Child) => {
            sleep(Duration::from_millis(10));
            map.insert(3, 3);
            assert_eq!(map.get(&2), Some(2));
        }
        Err(_) => {
            dbg!("Fork failed");
        }
    }
}
