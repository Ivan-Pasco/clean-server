# Fix: `_db_query` JSON Response Causes Array Access Bug

## Latest Finding (v1.7.3 - still broken)

```
Initial heap pointer from __heap_ptr global: 25336
WASM string.concat allocations: 25336, 25400, 25536... (sequential)
DB query result at: 407624 (HUGE GAP!)
```

**The server allocates at 407624 while WASM uses 25336.** This is a separate allocation path.

### The Real Problem

The server calls WASM's `malloc` to allocate memory for the DB result. The `malloc` returns `407624`. But then:
1. Server writes JSON string at `407624`
2. Server should update `__heap_ptr` to `409128` (407624 + 1504)
3. WASM's JSON parser then calls `malloc` for parsed objects
4. If `__heap_ptr` wasn't updated, `malloc` returns overlapping addresses!

### Debug Steps

Add logging to verify:
```rust
// Before write_string_to_caller
eprintln!("__heap_ptr BEFORE: {}", heap_ptr_global.get().unwrap_i32());

// After calling WASM malloc
eprintln!("malloc returned: {}", ptr);

// After updating __heap_ptr
eprintln!("__heap_ptr AFTER: {}", new_heap_ptr);
```

### Key Question

Is `write_string_to_caller` calling WASM's `malloc` export? If so, `malloc` should be updating `__heap_ptr` internally. If not, server needs to update it manually.

---

## Problem

When Clean Language code calls `_db_query()` to fetch data from SQLite, the returned JSON string works for the first array element but returns **wrong values** for subsequent elements.

### Symptoms

```cln
string result = _db_query("SELECT * FROM articles", "[]")
any data = json.tryTextToData(result)
any rows = data.data.rows

any item0 = rows.get(0)  // ✅ Returns correct object pointer (26976)
any item1 = rows.get(1)  // ❌ Returns "read_time" (a field name, NOT an object!)
```

### Debug Output

```
DEBUG: In loop, i = 0
DEBUG: Got article, article = 26976      <-- Correct pointer
DEBUG: slug = renaissance-of-analog      <-- Works!

DEBUG: In loop, i = 1
DEBUG: Got article, article = read_time  <-- WRONG! This is a field name string
DEBUG: About to access article.slug...
[CRASH]
```

## Key Finding

- ✅ **Hardcoded JSON works**: When JSON is a string literal in the code, `.get(1)` returns correct values
- ❌ **Database JSON fails**: When JSON comes from `_db_query()`, `.get(1)` returns garbage

This means the bug is NOT in the compiler's JSON parsing - it's in how `_db_query` returns data to WASM.

## What to Investigate

### 1. Memory Allocation for Response String

Check how `_db_query` allocates memory for the JSON response:
- Is it using the WASM module's allocator correctly?
- Is the string being written to a valid memory region?
- Is the string length being set correctly?

### 2. String Format

Clean Language uses length-prefixed strings:
```
[4 bytes: length][string bytes...]
```

Verify that `_db_query`:
1. Calculates the correct JSON string length
2. Writes the 4-byte length prefix
3. Writes the string content immediately after
4. Returns the correct pointer to the length prefix

### 3. Memory Overlap

The JSON response is ~2KB. Check if:
- The allocated memory region is large enough
- There's no overlap with other allocations
- The heap pointer is being updated correctly

## Relevant Files to Check

```
src/bridge.rs       - Bridge function implementations
src/runtime.rs      - WASM runtime and memory management
src/lib.rs          - Module initialization
```

Look for:
- `_db_query` function implementation
- How strings are written to WASM memory
- Memory allocation functions (`alloc`, `write_string`, etc.)

## Test Case

The failing example is at:
```
/Users/earcandy/Documents/Dev/Clean Language/clean-framework/examples/article-blog/app-db.cln
```

Run with:
```bash
cd /Users/earcandy/Documents/Dev/Clean Language/clean-framework/examples/article-blog
DATABASE_URL="sqlite://./blog.db" ~/.cleen/server/0.2.2/clean-server /tmp/app-db-latest.wasm --port 3030
curl http://localhost:3030/
```

## Expected Fix

After the fix:
1. `rows.get(0)` returns pointer to first object ✅ (already works)
2. `rows.get(1)` returns pointer to second object (currently broken)
3. `rows.get(2)` returns pointer to third object (currently broken)
4. `rows.get(3)` returns pointer to fourth object (currently broken)

## JSON Response Being Returned

The `_db_query` function returns this JSON (correctly formatted):

```json
{
  "data": {
    "count": 4,
    "rows": [
      {"slug": "renaissance-of-analog", "title": "The Renaissance of Analog", ...},
      {"slug": "designing-next-billion", "title": "Designing for the Next Billion", ...},
      {"slug": "battery-revolution", "title": "The Quiet Revolution in Batteries", ...},
      {"slug": "architecture-solitude", "title": "The Architecture of Solitude", ...}
    ]
  },
  "ok": true
}
```

The JSON content is correct - the issue is how it's being written to WASM memory.

## Priority

🔴 **CRITICAL** - This blocks any Clean Language app that iterates over database query results.
