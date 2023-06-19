use std::mem::size_of;
use std::thread::sleep;
use std::thread::spawn;
use std::time::Duration;

use shared_hashmap::Bucket;
use shared_hashmap::SharedMemoryHashMap;
use shared_hashmap::Error;

#[test]
fn test_insert() {
    let mut map = SharedMemoryHashMap::new(1024);
    map.insert(1, 1);
    assert_eq!(map.get(&1), Some(1));

    assert_eq!(map.get(&2), None);
    map.insert(2, 2);
    assert_eq!(map.get(&2), Some(2));

}

#[test]
fn test_insert_slice() {
    let mut map = SharedMemoryHashMap::new(1024);
    map.insert(1, [1, 2, 3]);
    map.insert(2, [3, 4, 5]);
    assert_eq!(map.get(&1), Some([1, 2, 3]));
    assert_eq!(map.get(&2), Some([3, 4, 5]));
}

#[test]
fn test_insert_string() {
    let mut map = SharedMemoryHashMap::new(1024);
    map.insert(1, String::from("Hello"));
    map.insert(2, String::from("World"));
    assert_eq!(map.get(&1), Some(String::from("Hello")));
    assert_eq!(map.get(&2), Some(String::from("World")));
}

#[test]
fn test_insert_too_long() {
    let mut map = SharedMemoryHashMap::new(128);
    let insert = map.try_insert(1, String::from("Hello").repeat(100));
    assert_eq!(insert, Err(Error::TooLargeError));

    let fixed_size = size_of::<u8>() + size_of::<usize>();
    let item_size = size_of::<Bucket<u8, u8>>() + 4;
    dbg!(item_size);
    let mut map = SharedMemoryHashMap::new(fixed_size + item_size);
    map.try_insert(1, 1).unwrap();
    let insert = map.try_insert(2, 2);
    assert_eq!(insert, Err(Error::TooLargeError));
}

#[test]
fn test_remove() {
    let mut map = SharedMemoryHashMap::new(1024);
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
    let mut map = SharedMemoryHashMap::new(1024);
    map.insert(1, 1);
    assert_eq!(map.contains_key(&1), true);
    assert_eq!(map.contains_key(&2), false);
}

#[test]
fn test_clear() {
    let mut map = SharedMemoryHashMap::new(1024);
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
    let mut map = SharedMemoryHashMap::new(1024);
    let mut map2 = map.clone();
    map.insert(1, 1);
    assert_eq!(map2.get(&1), Some(1));

    map2.clear();
    assert_eq!(map.len(), 0);
    assert_eq!(map2.len(), 0);
}

#[test]
fn test_used() {
    let mut map = SharedMemoryHashMap::new(1024);
    map.insert(1, 1);
    assert_eq!(map.used(), 20);
    map.insert(2, 2);
    assert_eq!(map.used(), 40);
}

#[test]
fn test_free() {
    let mut map = SharedMemoryHashMap::new(1024);
    map.insert(1, 1);
    assert_eq!(map.free(), 1024 - 20);
}

#[test]
fn test_race_condition() {
    let mut map = SharedMemoryHashMap::new(1024 * 1024);
    map.insert(1, 1);
    map.insert(2, 2);
    map.insert(3, 3);

    let mut map2 = map.clone();
    let mut map3 = map.clone();

    spawn(move || {
        for i in 100..2000 {
            dbg!(map2.used());
            //map2.dump_data();
            map2.try_insert(i, i).unwrap();
        }
    });
    spawn(move || {
        for i in 2000..3000 {
            dbg!(map3.used());
            //map3.dump_data();
            map3.try_insert(i, i).unwrap();
        }
    });

    sleep(Duration::from_millis(100));
    //assert_eq!(map.len(), 1903);
    //dbg!(map);
}
