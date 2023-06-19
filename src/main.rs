#![feature(pointer_byte_offsets)]

use std::marker::PhantomData;
use std::ptr::slice_from_raw_parts;
use raw_sync::locks::{Mutex, LockInit, LockImpl};

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
    fn set_value( &mut self, value: V) {
        let value = bincode::serialize(&value).unwrap();
        dbg!(value.len());
        self.value_size = value.len();
        unsafe {
            let ptr = (self as *mut Bucket<K, V>).add(1) as *mut u8;
            core::ptr::copy(value.as_ptr(), ptr as *mut u8, value.len());
        }
    }
}

impl<K, V:> Bucket<K, V> {
    fn next(&self) -> *mut Bucket<K, V> {
        unsafe { (self as *const Bucket<K, V>).add(1).byte_add(self.value_size) as *mut Bucket<K, V> }
    }
}

#[derive(PartialEq, Debug)]
pub enum Error {
    TooLargeError,
}

pub struct LockGuard(*mut u8);

impl LockGuard {
    fn new(lock: *mut u8) -> Self {
        unsafe {
            while *lock != 0 {
                std::thread::yield_now();
            }
            *lock = 1;
        }
        Self(lock)
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        unsafe { *self.0 = 0 }
    }
}

struct SharedMemoryContents {
    bucket_count: usize,
    used: usize,
}

unsafe impl<K: Send, V> Send for SharedMemoryHashMap<K, V> {}

///
/// The layout of the shared memory is
/// [lock: u8][bucket_count: usize][used: usize][Bucket][value data][Bucket][value data]...
pub struct SharedMemoryHashMap<K, V> {
    shm: Shmem,
    bucket_count: *mut usize,
    lock: Box<dyn LockImpl>,
    used: *mut usize,
    end: *mut u8,
    buckets: *mut Bucket<K, V>,
}

impl<K: PartialEq + std::fmt::Debug, V: serde::Serialize + serde::Deserialize<'static>> std::fmt::Debug for SharedMemoryHashMap<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.bucket_iter().for_each(|b| {
            f.debug_struct("Bucket").field("key", &b.key).finish();
        });
        f.debug_struct("SharedMemoryHashMap")
            .field("buckets", &self.buckets)
            .field("bucket_count", unsafe { &*self.bucket_count } )
            //.field("lock", &self.lock.)
            .field("end", &self.end)
            .finish()
    }
}

impl<K: PartialEq, V: serde::Serialize + serde::Deserialize<'static>> SharedMemoryHashMap<K, V> {
    pub fn new(size: usize) -> Self {
        let shm_conf = ShmemConf::default().size(size);
        let shm = shm_conf.create().unwrap();
        let ptr = shm.as_ptr();
        let size = shm.len();
        Self {
            shm,
            lock: unsafe { Mutex::new(ptr as *mut u8, ptr as *mut u8).unwrap().0 },
            bucket_count: unsafe { ptr.add(1) as *mut usize},
            used: unsafe { (ptr.add(1) as *mut usize).add(1) as *mut usize },
            buckets: unsafe { (ptr.add(1) as *mut usize).add(2) as *mut Bucket<K, V> },
            end: unsafe { ptr.add(size) },
        }
    }

    fn bucket_iter(&self) -> SharedMemoryHashMapIter<K, V> {
        SharedMemoryHashMapIter {
            current: 0,
            current_ptr: self.buckets,
            hashmap: self,
        }
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

        let _lock = self.lock();

        let ptr = unsafe { self.buckets.byte_add(self.used()) };
        let last = self.bucket_iter().last();
        let ptr = match last {
            Some(last) => last.next(),
            None => self.buckets,
        };
        if unsafe { ptr.add(1).byte_add(value_size) as *mut u8 } as usize > self.end as usize {
            dbg!(last.unwrap().value_size);
            dbg!(self.end as usize - self.buckets as usize);
            unsafe { dbg!(value_size, ptr, ptr.add(1).byte_add(value_size) as usize - self.buckets as usize, self.end); }
            return Err(Error::TooLargeError);
        }
        unsafe {
            core::ptr::copy(&bucket, ptr, 1);
            *self.bucket_count += 1;
            (*ptr).set_value(value);
            *self.used = (*ptr).next() as usize - self.buckets as usize;
        }
        Ok(())
    }

    pub fn get(&mut self, key: &K) -> Option<V> {
        let _lock = self.lock();
        for bucket in self.bucket_iter() {
            if bucket.key == *key {
                return bucket.get_value();
            }
        }
        None
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let mut current_index = 0;
        let mut current_ptr = self.buckets;
        let mut next_bucket: *mut Bucket<K, V> = std::ptr::null_mut();

        let _lock = self.lock();

        while current_index < self.len() {
            let bucket = unsafe { &*current_ptr };

            if current_index + 1 < self.len() {
                next_bucket = bucket.next()
            }
            if &bucket.key == key {
                unsafe {
                    *self.bucket_count -= 1;
                    // Shift all the next memory back.
                    if !next_bucket.is_null() {
                        std::ptr::copy(
                            next_bucket as *const u8,
                            current_ptr as *mut u8,
                            self.end as usize - next_bucket as usize,
                        );
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
        unsafe { *self.bucket_count }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn used(&self) -> usize {
        unsafe { *self.used }
    }

    pub fn free(&self) -> usize {
        unsafe { self.shm.len() - *self.used }
    }

    pub fn contains_key(&mut self, key: &K) -> bool {
        let _lock = self.lock();
        for bucket in self.bucket_iter() {
            if bucket.key == *key {
                return true;
            }
        }
        false
    }

    pub fn clear(&mut self) {
        let lock = self.lock().unwrap();
        let mut data = unsafe { &mut *(*lock as *mut SharedMemoryContents) };
        data.bucket_count = 1;
        unsafe {
            *self.bucket_count = 0;
        }
    }

    pub fn lock(&self) -> Result<raw_sync::locks::LockGuard<'_>, Box<dyn std::error::Error>> {
        self.lock.lock()
    }

    pub fn dump_data(&self) -> Vec<u8> {
        for i in 0..500 {
            unsafe {
                dbg!( self.buckets.byte_add(i) as *mut u8, *(self.buckets.byte_add(i) as *mut u8 ));
            }
        }
        unsafe {
            &*slice_from_raw_parts(self.buckets as _, 20)
        }.to_vec()
    }
}

impl<K: PartialEq, V: serde::Serialize + serde::Deserialize<'static>> Clone
    for SharedMemoryHashMap<K, V>
{
    fn clone(&self) -> Self {
        let shm_conf = ShmemConf::default().size(self.shm.len());
        let shm = shm_conf.os_id(self.shm.get_os_id()).open().unwrap();
        let ptr = shm.as_ptr();
        let size = shm.len();
        Self {
            shm,
            lock: unsafe { Mutex::new(ptr as *mut u8, ptr as *mut u8).unwrap().0 },
            bucket_count: unsafe { ptr.add(1) as *mut usize},
            used: unsafe { (ptr.add(1) as *mut usize).add(1) as *mut usize },
            buckets: unsafe { (ptr.add(1) as *mut usize).add(2) as *mut Bucket<K, V> },
            end: unsafe { ptr.add(size) },
        }
    }
}
pub struct SharedMemoryHashMapIter<'a, K, V> {
    current: usize,
    current_ptr: *mut Bucket<K, V>,
    hashmap: &'a SharedMemoryHashMap<K, V>,
}

impl<'a, K, V> Iterator for SharedMemoryHashMapIter<'a, K, V> {
    type Item = &'a Bucket<K, V>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current < unsafe { *self.hashmap.bucket_count } {
            let item = unsafe { &*self.current_ptr };
            self.current += 1;
            self.current_ptr = item.next();
            unsafe { dbg!((*self.current_ptr).value_size) };
            Some(item)
        } else {
            None
        }
    }
}

fn main() {
    let mut map: SharedMemoryHashMap<u8, u8> = SharedMemoryHashMap::new(1024);

    // map.iter().for_each(|bucket| {
    //     println!("{:?}", bucket.value);
    // });
}
