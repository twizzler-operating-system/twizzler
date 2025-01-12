# lru-mem

An implementation of a memory-bounded LRU (least-recently-used) cache for Rust.
It supports average-case O(1) insert, get, and remove. There are also
additional utility methods such as iterators, capacity management, and mutable
access.

Note that the memory required for each entry is only an estimate and some
auxiliary structure is disregarded. Therefore, the actual data structure can
take more memory than was assigned, however this should not be an excessive
amount in most cases.

# Motivating example

Imagine we are building a web server that sends large responses to clients. To
reduce the load, they are split into sections and the client is given a token
to access the different sections individually. However, recomputing the
sections on each request leads to too much server load, so they need to be
cached. An LRU cache is useful in this situation, as clients are most likely to
request new sections temporally localized.

Now consider the situation when most responses are very small, but some may be
large. This would either lead to the cache being conservatively sized and allow
for less cached responses than would normally be possible, or to the cache
being liberally sized and potentially overflow memory if too many large
responses have to be cached. To prevent this, the cache is designed with an
upper bound on its memory instead of the number of elements.

The code below shows how the basic structure might look like.

```rust
use lru_mem::LruCache;

struct WebServer {
    cache: LruCache<u128, Vec<String>>
}

fn random_token() -> u128 {
    // A cryptographically secure random token.
    42
}

fn generate_sections(input: String) -> Vec<String> {
    // A complicated set of sections that is highly variable in size.
    vec![input.clone(), input]
}

impl WebServer {
    fn new(max_size: usize) -> WebServer {
        // Create a new web server with a cache that holds at most max_size
        // bytes of elements.
        WebServer {
            cache: LruCache::new(max_size)
        }
    }

    fn on_query(&mut self, input: String) -> u128 {
        // Generate sections, store them in the cache, and return token.
        let token = random_token();
        let sections = generate_sections(input);
        self.cache.insert(token, sections)
            .expect("sections do not fit in the cache");

        token
    }

    fn on_section_request(&mut self, token: u128, index: usize)
            -> Option<&String> {
        // Lookup the token and get the section with given index.
        self.cache.get(&token).and_then(|s| s.get(index))
    }
}
```

For more details, check out the documentation.

# Links

* [Crate](https://crates.io/crates/lru-mem)
* [Documentation](https://docs.rs/lru-mem/)
* [Repository](https://github.com/florian1345/lru-mem)
