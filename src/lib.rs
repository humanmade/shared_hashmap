use raw_sync::locks::{LockGuard, LockImpl, LockInit, Mutex};
use std::fmt::Formatter;
use std::ptr::slice_from_raw_parts;
use std::time::Instant;
use std::{marker::PhantomData, mem::size_of};

use shared_memory::{Shmem, ShmemConf};

fn round_to_boundary<T>(num: usize) -> usize {
    let multiple = std::mem::align_of::<T>();
    if num % multiple == 0 {
        num
    } else {
        num + multiple - num % multiple
    }
}

/// After the bucket we store the memory: [Bucket<K, V>][key_data][value_data]
#[derive(Debug)]
pub struct Bucket<K, V> {
    value_size: usize,
    key_size: usize,
    phantom: PhantomData<(K, V)>,
    last_accessed: Instant,
}

impl<K: serde::Serialize + serde::Deserialize<'static>, V: serde::Serialize + serde::Deserialize<'static>>
    Bucket<K, V>
{
    fn get_value(&self) -> V {
        unsafe { bincode::deserialize(&*slice_from_raw_parts(self.value_ptr(), self.value_size)).unwrap() }
    }
    fn set_value(&mut self, value: V) {
        let value = bincode::serialize(&value).unwrap();
        self.value_size = value.len();
        unsafe {
            core::ptr::copy(value.as_ptr(), self.value_ptr(), value.len());
        }
    }
    fn get_key(&self) -> K {
        unsafe { bincode::deserialize(&*slice_from_raw_parts(self.key_ptr(), self.key_size)).unwrap() }
    }
    fn set_key(&mut self, key: K) {
        let key = bincode::serialize(&key).unwrap();
        self.key_size = key.len();
        unsafe {
            core::ptr::copy(key.as_ptr(), self.key_ptr(), key.len());
        }
    }
}

impl<K, V> Bucket<K, V> {
    fn key_ptr(&self) -> *mut u8 {
        unsafe { (self as *const Bucket<K, V>).add(1) as *mut u8 }
    }
    fn value_ptr(&self) -> *mut u8 {
        // To add a specific amount of bytes, instead of using `byte_add()` which is not in stable Rust yet,
        // we have to cash to a pointer of u8 and use .add().
        unsafe { ((self as *const Bucket<K, V>).add(1) as *mut u8).add(self.key_size) }
    }
    fn next(&self) -> *mut Bucket<K, V> {
        unsafe {
            let ptr = (self as *const Bucket<K, V>).add(1) as *mut u8;
            let ptr = ptr.add(round_to_boundary::<Bucket<K, V>>(self.value_size + self.key_size));
            ptr as *mut Bucket<K, V>
        }
    }
}

#[derive(Debug)]
pub enum Error {
    TooLargeError,
    ShmemError(shared_memory::ShmemError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::TooLargeError => "The hashmap is too large to insert the new key-value pair.".fmt(f),
            Error::ShmemError(e) => e.to_string().fmt(f),
        }
    }
}
pub struct SharedMemoryContents<K, V> {
    bucket_count: usize,
    /// The amount of data that is used in buckets data / contents.
    used: usize,
    phantom: PhantomData<(K, V)>,
    size: usize,
}

impl<
        K: std::fmt::Debug + PartialEq + serde::Serialize + serde::Deserialize<'static>,
        V: std::fmt::Debug + serde::Serialize + serde::Deserialize<'static>,
    > SharedMemoryContents<K, V>
{
    fn bucket_iter(&self) -> SharedMemoryHashMapBucketIter<K, V> {
        SharedMemoryHashMapBucketIter {
            current: 0,
            current_ptr: self.buckets_ptr(),
            map: self,
        }
    }

    fn buckets_ptr(&self) -> *mut Bucket<K, V> {
        unsafe { (self as *const SharedMemoryContents<K, V>).add(1) as *mut Bucket<K, V> }
    }

    /// Tries to insert a key-value pair into the shared hashmap. If the key already exists, it is removed before inserting the new value.
    /// If the size of the hashmap exceeds the maximum size, it will evict items until there is enough space to insert the new key-value pair.
    /// Returns a Result containing an Option of the removed value if the hashmap was full and an item had to be evicted, or an error if the hashmap is too large to insert the new key-value pair.
    ///
    /// # Arguments
    ///
    /// * `key` - A key of type K to insert into the hashmap.
    /// * `value` - A value of type V to insert into the hashmap.
    ///
    /// ```
    pub fn try_insert(&mut self, key: K, value: V) -> Result<Option<V>, Error> {
        let value_size = bincode::serialized_size(&value).unwrap() as usize;
        let key_size = bincode::serialized_size(&key).unwrap() as usize;
        let bucket = Bucket {
            phantom: PhantomData::<(K, V)>,
            value_size: 0,
            key_size: 0,
            last_accessed: Instant::now(),
        };
        if size_of::<Bucket<K, V>>() + value_size + key_size > self.size {
            return Err(Error::TooLargeError);
        }

        if self.contains_key(&key) {
            self.remove(&key);
        }
        let mut removed = None;
        while self.used() + size_of::<Bucket<K, V>>() + value_size + key_size > self.size {
            match self.evict() {
                Some(item) => removed = Some(item),
                None => {
                    return Err(Error::TooLargeError);
                }
            }
        }

        let ptr = unsafe { (self.buckets_ptr() as *mut u8).add(self.used) as *mut Bucket<K, V> };
        unsafe {
            core::ptr::copy(&bucket, ptr, 1);
            self.bucket_count += 1;
            // Key must come before value
            (*ptr).set_key(key);
            (*ptr).set_value(value);
            let extra_size = round_to_boundary::<Bucket<K, V>>((*ptr).value_size + (*ptr).key_size);
            self.used += size_of::<Bucket<K, V>>() + extra_size;
        }

        Ok(removed)
    }

    /// Returns the value associated with the given key in the hashmap, or `None` if the key is not present.
    /// If the key is present, the last accessed time for the corresponding bucket is updated.
    ///
    /// # Arguments
    ///
    /// * `key` - A reference to the key to search for in the hashmap.
    ///
    /// # Returns
    ///
    /// * `Some(V)` - The value associated with the given key, if it exists in the hashmap.
    /// * `None` - If the key is not present in the hashmap.
    pub fn get(&mut self, key: &K) -> Option<V> {
        for bucket in self.bucket_iter() {
            if bucket.get_key() == *key {
                bucket.last_accessed = Instant::now();
                return Some(bucket.get_value());
            }
        }
        None
    }

    /// Returns the value associated with the given key, if it exists in the hashmap.
    ///
    /// # Arguments
    ///
    /// * `key` - A reference to the key to search for in the hashmap.
    ///
    /// # Returns
    ///
    /// * `Some(V)` - The value associated with the given key, if it exists in the hashmap.
    /// * `None` - If the key does not exist in the hashmap.
    pub fn peak(&self, key: &K) -> Option<V> {
        for bucket in self.bucket_iter() {
            if bucket.get_key() == *key {
                return Some(bucket.get_value());
            }
        }
        None
    }

    /// Removes the entry with the specified key from the hashmap and returns the value of the removed entry.
    /// If the key is not present in the hashmap, returns `None`.
    ///
    /// # Arguments
    ///
    /// * `self` - A mutable reference to the hashmap.
    /// * `key` - The key of the entry to be removed.
    ///
    /// # Returns
    /// * `Some(V)` - The value of the removed entry, if it existed in the hashmap.
    /// * `None` - If the key was not present in the hashmap.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let mut current_index = 0;
        let mut current_ptr = self.buckets_ptr();
        let mut next_bucket: *mut Bucket<K, V> = std::ptr::null_mut();

        while current_index < self.len() {
            let bucket = unsafe { &*current_ptr };

            if current_index + 1 < self.len() {
                next_bucket = bucket.next()
            }
            if &bucket.get_key() == key {
                self.bucket_count -= 1;
                self.used -=
                    size_of::<Bucket<K, V>>() + round_to_boundary::<Bucket<K, V>>(bucket.value_size + bucket.key_size);
                unsafe {
                    // Shift all the next memory back.
                    if !next_bucket.is_null() {
                        std::ptr::copy(next_bucket as *const u8, current_ptr as *mut u8, self.used);
                    }
                }
                return Some(bucket.get_value());
            }
            current_ptr = bucket.next();
            current_index += 1;
        }
        None
    }

    /// Returns the number of elements in the shared hashmap.
    pub fn len(&self) -> usize {
        self.bucket_count
    }

    /// Returns `true` if the shared hashmap contains no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the total size of the shared memory contents used by the hashmap.
    pub fn used(&self) -> usize {
        self.used + size_of::<SharedMemoryContents<K, V>>()
    }

    /// Returns the number of free slots in the hashmap.
    pub fn free(&self) -> usize {
        self.size - self.used()
    }

    /// Returns true if the shared hashmap contains the specified key.
    ///
    /// # Arguments
    ///
    /// * `key` - A reference to the key to search for in the shared hashmap.
    ///
    pub fn contains_key(&self, key: &K) -> bool {
        for bucket in self.bucket_iter() {
            if bucket.get_key() == *key {
                return true;
            }
        }
        false
    }

    /// Clears the hashmap, removing all key-value pairs.
    pub fn clear(&mut self) {
        self.bucket_count = 0;
        self.used = 0;
    }

    /// Get the least recently used bucket.
    pub fn get_lru(&self) -> Option<&Bucket<K, V>> {
        let mut oldest_bucket: Option<&Bucket<K, V>> = None;

        for bucket in self.bucket_iter() {
            match oldest_bucket {
                None => {
                    oldest_bucket = Some(bucket);
                }
                Some(oldest) => {
                    if bucket.last_accessed < oldest.last_accessed {
                        oldest_bucket = Some(bucket);
                    }
                }
            }
        }
        oldest_bucket
    }

    /// Evict the least recently used bucket.
    pub fn evict(&mut self) -> Option<V> {
        let bucket = self.get_lru();
        if let Some(bucket) = bucket {
            // Get around the borrow checker... oh dear.
            let to_remove = bucket as *const Bucket<K, V>;
            return self.remove(&unsafe { &*to_remove }.get_key());
        }
        None
    }
}

impl<
        K: std::fmt::Debug + PartialEq + serde::Serialize + serde::Deserialize<'static>,
        V: std::fmt::Debug + serde::Serialize + serde::Deserialize<'static>,
    > std::fmt::Debug for SharedMemoryContents<K, V>
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedMemoryContents")
            .field("bucket_count", &self.bucket_count)
            .field("used", &self.used)
            .field("size", &self.size)
            .finish()
    }
}

pub struct SharedMemoryHashMapBucketIter<'a, K, V> {
    current: usize,
    current_ptr: *mut Bucket<K, V>,
    map: &'a SharedMemoryContents<K, V>,
}

impl<'a, K, V> Iterator for SharedMemoryHashMapBucketIter<'a, K, V> {
    type Item = &'a mut Bucket<K, V>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current < self.map.bucket_count {
            let item = unsafe { &mut *self.current_ptr };
            self.current += 1;
            self.current_ptr = item.next();
            Some(item)
        } else {
            None
        }
    }
}

pub struct SharedMemoryHashMapIter<'a, K, V> {
    current: usize,
    current_ptr: *mut Bucket<K, V>,
    map: &'a SharedMemoryContents<K, V>,
    #[allow(dead_code)]
    lock: LockGuard<'a>,
}

impl<'a, K: serde::Serialize + serde::Deserialize<'static>, V: serde::Serialize + serde::Deserialize<'static>> Iterator
    for SharedMemoryHashMapIter<'a, K, V>
{
    type Item = (K, V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.current < self.map.bucket_count {
            let item = unsafe { &mut *self.current_ptr };
            self.current += 1;
            self.current_ptr = item.next();
            Some((item.get_key(), item.get_value()))
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

impl<
        K: std::fmt::Debug + PartialEq + serde::Serialize + serde::Deserialize<'static>,
        V: std::fmt::Debug + serde::Serialize + serde::Deserialize<'static>,
    > std::fmt::Debug for SharedMemoryHashMap<K, V>
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut builder = f.debug_struct("SharedMemoryHashMap");

        builder.field("os_id", &self.shm.get_os_id().to_string());
        builder.field("len", &self.len().to_string());
        builder.field("contents", &self.iter().collect::<Vec<_>>());
        builder.finish()
    }
}

impl<
        K: std::fmt::Debug + PartialEq + serde::Serialize + serde::Deserialize<'static>,
        V: std::fmt::Debug + serde::Serialize + serde::Deserialize<'static>,
    > SharedMemoryHashMap<K, V>
{
    pub fn new(size: usize) -> Result<Self, Error> {
        let shm_conf = ShmemConf::default().size(size);
        let shm = shm_conf.create().unwrap();
        let ptr = shm.as_ptr();
        if size < Mutex::size_of(Some(ptr)) + size_of::<SharedMemoryContents<K, V>>() {
            return Err(Error::TooLargeError);
        }
        let size = shm.len() - Mutex::size_of(Some(ptr));
        let hashmap = Self {
            shm,
            lock: unsafe { Mutex::new(ptr, ptr.add(Mutex::size_of(Some(ptr)))).unwrap().0 },
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

        Ok(hashmap)
    }

    pub fn iter(&self) -> SharedMemoryHashMapIter<K, V> {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };

        SharedMemoryHashMapIter {
            current: 0,
            current_ptr: contents.buckets_ptr(),
            map: contents,
            lock,
        }
    }

    pub fn lock(&self) -> Result<raw_sync::locks::LockGuard<'_>, Box<dyn std::error::Error>> {
        self.lock.lock()
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        match self.try_insert(key, value) {
            Ok(option) => option,
            Err(_) => None,
        }
    }

    pub fn try_insert(&mut self, key: K, value: V) -> Result<Option<V>, Error> {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.try_insert(key, value)
    }

    pub fn get(&mut self, key: &K) -> Option<V> {
        let lock = self.lock().unwrap();
        let contents = unsafe { &mut *(*lock as *mut SharedMemoryContents<K, V>) };
        contents.get(key)
    }

    pub fn peak(&self, key: &K) -> Option<V> {
        let lock = self.lock().unwrap();
        let contents = unsafe { &*(*lock as *mut SharedMemoryContents<K, V>) };
        contents.peak(key)
    }

    pub fn get_lru(&self) -> Option<(K, V)> {
        let lock = self.lock().unwrap();
        let contents = unsafe { &*(*lock as *mut SharedMemoryContents<K, V>) };
        contents.get_lru().map(|bucket| (bucket.get_key(), bucket.get_value()))
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
        contents.used() + Mutex::size_of(Some(*lock))
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

    pub fn size_of(key: &K, value: &V) -> usize {
        let value_size = bincode::serialized_size(value).unwrap() as usize;
        let key_size = bincode::serialized_size(key).unwrap() as usize;
        size_of::<Bucket<K, V>>() + round_to_boundary::<Bucket<K, V>>(value_size + key_size)
    }

    pub fn try_clone(&self) -> Result<Self, Error> {
        let shm_conf = ShmemConf::default().size(self.shm.len());
        let shm = shm_conf.os_id(self.shm.get_os_id()).open().map_err(Error::ShmemError)?;
        let ptr = shm.as_ptr();
        Ok(Self {
            shm,
            lock: unsafe { Mutex::from_existing(ptr, ptr.add(Mutex::size_of(Some(ptr)))).unwrap().0 },
            phantom: PhantomData::<(K, V)>,
        })
    }
}
