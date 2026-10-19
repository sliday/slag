# batch-loader

Batch loading helper for the detail views. `fetchAll(ids, loader)`
runs `loader(id)` for every id and resolves with the results in the
same order as `ids`.

## Contract

- The result array lines up with `ids`: result[i] is the value for
  ids[i], no matter which load finishes first.
- All loads run concurrently; slow loads must not reorder fast ones.
- An empty id list resolves to an empty array.

## Tests

    npm test
