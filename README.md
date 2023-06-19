# Shared HashMap

A shared memory hashmap including LRU eviction.

This crate provides a shared hashmap limited by memory size that can be used between difference processes and thread. This is achieved by using the [shared_memory](https://github.com/elast0ny/shared_memory) crate and [raw_sync-rs](https://github.com/elast0ny/raw_sync-rs) for IPC mutexes.

The `SharedMemoryHashMap` internally handles locking and serializing of Keys and Values in an optimized memory layout. Current the LRU implementation is basic, using timestamps for access rather than an ordered key-list. Implementing a key-list for LRU purposes is possible but requires more manual memory management.

## Usage

```rust
use shared_memory_hashmap::SharedMemoryHashMap;

fn main() {
    let mut map = SharedMemoryHashMap::new(1024); // bytes.
    map.insert(1, "Hello");
    map.insert(2, "World");

    map2 = map.clone(); // map2 uses the same shared memory block as `map`.
    spawn(|| move {
        map2.insert(3, "Goodbye");
    });
}
```
