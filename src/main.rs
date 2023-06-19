#![feature(pointer_byte_offsets)]

use raw_sync::locks::{LockImpl, LockInit, Mutex};
use std::ptr::slice_from_raw_parts;
use std::{marker::PhantomData, mem::size_of};

use shared_memory::{Shmem, ShmemConf};

#[derive(Debug)]
pub struct Bucket<K, V> {
    key: K,
    value_size: usize,
    phantom: PhantomData<V>,
}

impl<K, V: serde::Serialize + serde::Deserialize<'static>> Bucket<K, V> {
    fn get_value(&self) -> Option<V> {
        return unsafe {
            let value = (self as *const Bucket<K, V>).add(1) as *mut u8;
            bincode::deserialize(&*slice_from_raw_parts(value, self.value_size)).ok()
        };
    }
    fn set_value(&mut self, value: V) {
        let value = bincode::serialize(&value).unwrap();
        self.value_size = value.len();
        unsafe {
            let ptr = (self as *mut Bucket<K, V>).add(1) as *mut u8;
            core::ptr::copy(value.as_ptr(), ptr as *mut u8, value.len());
        }
    }
}

impl<K, V> Bucket<K, V> {
    fn next(&self) -> *mut Bucket<K, V> {
        unsafe {
            (self as *const Bucket<K, V>)
                .add(1)
                .byte_add(self.value_size) as *mut Bucket<K, V>
        }
    }
}

#[derive(PartialEq, Debug)]
pub enum Error {
    TooLargeError,
}
struct SharedMemoryContents<K, V> {
    bucket_count: usize,
    used: usize,
    phantom: PhantomData<(K, V)>,
    size: usize,
}

impl<K: PartialEq, V: serde::Serialize + serde::Deserialize<'static>> SharedMemoryContents<K, V> {
    fn bucket_iter(&self) -> SharedMemoryHashMapIter<K, V> {
        SharedMemoryHashMapIter {
            current: 0,
            current_ptr: self.buckets_ptr(),
            map: self,
        }
    }

    fn buckets_ptr(&self) -> *mut Bucket<K, V> {
        unsafe { (self as *const SharedMemoryContents<K, V>).add(1) as *mut Bucket<K, V> }
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<()> {
        self.try_insert(key, value).ok()
    }

    pub fn try_insert(&mut self, key: K, value: V) -> Result<(), Error> {
        let value_size = bincode::serialized_size(&value).unwrap() as usize;
        let bucket = Bucket {
            key,
            phantom: PhantomData::<V>,
            value_size: 0,
        };
        let ptr = unsafe { self.buckets_ptr().byte_add(self.used) };

        if self.used + size_of::<Bucket<K, V>>() + value_size > self.size {
            return Err(Error::TooLargeError);
        }
        unsafe {
            core::ptr::copy(&bucket, ptr, 1);
            self.bucket_count += 1;
            (*ptr).set_value(value);
            self.used += size_of::<Bucket<K, V>>() + (*ptr).value_size;
        }
        Ok(())
    }

    pub fn get(&mut self, key: &K) -> Option<V> {
        for bucket in self.bucket_iter() {
            if bucket.key == *key {
                return bucket.get_value();
            }
        }
        None
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let mut current_index = 0;
        let mut current_ptr = self.buckets_ptr();
        let mut next_bucket: *mut Bucket<K, V> = std::ptr::null_mut();

        while current_index < self.len() {
            let bucket = unsafe { &*current_ptr };

            if current_index + 1 < self.len() {
                next_bucket = bucket.next()
            }
            if &bucket.key == key {
                self.bucket_count -= 1;
                self.used -= size_of::<Bucket<K, V>>() + bucket.value_size;
                unsafe {
                    // Shift all the next memory back.
                    if !next_bucket.is_null() {
                        std::ptr::copy(next_bucket as *const u8, current_ptr as *mut u8, self.used);
                    }
                }
                break;
            }
            current_ptr = bucket.next();
            current_index += 1;
        }
        None
    }

    pub fn len(&self) -> usize {
        self.bucket_count
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn used(&self) -> usize {
        self.used
    }

    pub fn free(&self) -> usize {
        self.size - self.used
    }

    pub fn contains_key(&self, key: &K) -> bool {
        for bucket in self.bucket_iter() {
            if bucket.key == *key {
                return true;
            }
        }
        false
    }

    pub fn clear(&mut self) {
        self.bucket_count = 0;
    }
}

pub struct SharedMemoryHashMapIter<'a, K, V> {
    current: usize,
    current_ptr: *mut Bucket<K, V>,
    map: &'a SharedMemoryContents<K, V>,
}

impl<'a, K, V> Iterator for SharedMemoryHashMapIter<'a, K, V> {
    type Item = &'a Bucket<K, V>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current < self.map.bucket_count {
            let item = unsafe { &*self.current_ptr };
            self.current += 1;
            self.current_ptr = item.next();
            Some(item)
        } else {
            None
        }
    }
}

unsafe impl<K: Send, V> Send for SharedMemoryHashMap<K, V> {}

///
/// The layout of the shared memory is
/// [lock: u8][bucket_count: usize][used: usize][Bucket][value data][Bucket][value data]...
pub struct SharedMemoryHashMap<K, V> {
    shm: Shmem,
    lock: Box<dyn LockImpl>,
    phantom: PhantomData<(K, V)>,
}

// impl<K: PartialEq + std::fmt::Debug, V: serde::Serialize + serde::Deserialize<'static>> std::fmt::Debug for SharedMemoryHashMap<K, V> {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//         self.bucket_iter().for_each(|b| {
//             f.debug_struct("Bucket").field("key", &b.key).finish();
//         });
//         f.debug_struct("SharedMemoryHashMap")
//             .field("buckets", &self.buckets)
//             .field("bucket_count", unsafe { &*self.bucket_count } )
//             //.field("lock", &self.lock.)
//             .field("end", &self.end)
//             .finish()
//     }
// }

impl<K: PartialEq, V: serde::Serialize + serde::Deserialize<'static>> SharedMemoryHashMap<K, V> {
    pub fn new(size: usize) -> Self {
        let shm_conf = ShmemConf::default().size(size);
        let shm = shm_conf.create().unwrap();
        let ptr = shm.as_ptr();
        let size = shm.len();
        let hashmap = Self {
            shm,
            lock: unsafe {
                Mutex::new(
                    ptr as *mut u8,
                    (ptr as *mut u8).byte_add(Mutex::size_of(Some(ptr))),
                )
                .unwrap()
                .0
            },
            phantom: PhantomData::<(K, V)>,
        };

        // Initialize the hashmap.
        let contents: SharedMemoryContents<K, V> = SharedMemoryContents {
            bucket_count: 0,
            used: 0,
            phantom: PhantomData,
            size,
        };
        {
            let lock = hashmap.lock().unwrap();
            unsafe {
                core::ptr::copy(&contents, *lock as *mut SharedMemoryContents<K, V>, 1);
            }
        }

        hashmap
    }

    pub fn lock(&self) -> Result<raw_sync::locks::LockGuard<'_>, Box<dyn std::error::Error>> {
        self.lock.lock()
    }

    // pub fn dump_data(&self) -> Vec<u8> {
    //     for i in 0..500 {
    //         unsafe {
    //             dbg!( self.buckets.byte_add(i) as *mut u8, *(self.buckets.byte_add(i) as *mut u8 ));
    //         }
    //     }
    //     unsafe {
    //         &*slice_from_raw_parts(self.buckets as _, 20)
    //     }.to_vec()
    // }

    pub fn insert(&mut self, key: K, value: V) -> Option<()> {
        self.try_insert(key, value).ok()
    }

    pub fn try_insert(&mut self, key: K, value: V) -> Result<(), Error> {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.try_insert(key, value)
    }

    pub fn get(&mut self, key: &K) -> Option<V> {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.get(key)
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.remove(key)
    }

    pub fn len(&self) -> usize {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.len()
    }

    pub fn is_empty(&self) -> bool {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.is_empty()
    }

    pub fn used(&self) -> usize {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.used()
    }

    pub fn free(&self) -> usize {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.free()
    }

    pub fn contains_key(&mut self, key: &K) -> bool {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.contains_key(key)
    }

    pub fn clear(&mut self) {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.clear()
    }
}

impl<K: PartialEq, V: serde::Serialize + serde::Deserialize<'static>> Clone
    for SharedMemoryHashMap<K, V>
{
    fn clone(&self) -> Self {
        let shm_conf = ShmemConf::default().size(self.shm.len());
        let shm = shm_conf.os_id(self.shm.get_os_id()).open().unwrap();
        let ptr = shm.as_ptr();
        Self {
            shm,
            lock: unsafe {
                Mutex::from_existing(
                    ptr as *mut u8,
                    (ptr as *mut u8).byte_add(Mutex::size_of(Some(ptr))),
                )
                .unwrap()
                .0
            },
            phantom: PhantomData::<(K, V)>,
        }
    }
}

fn main() {
    let mut map: SharedMemoryHashMap<u8, u8> = SharedMemoryHashMap::new(1024);

    // map.iter().for_each(|bucket| {
    //     println!("{:?}", bucket.value);
    // });
}
