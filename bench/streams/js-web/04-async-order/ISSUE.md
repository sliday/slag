# Batch results come back scrambled when one record is slow

Our detail view loads records in batch with this repo. When one record
is slow, the results land in the wrong slots and the view shows the
wrong data under each row.

I reproduced it with three ids and a loader that resolves each id as
its uppercase, with a fake delay per id:

    ids: ["a", "b", "c"]
    delays: a takes 30ms, b takes 5ms, c takes 10ms

Expected: fetchAll resolves with ["A", "B", "C"], matching the order
of the ids I passed in.

Actual: fetchAll resolves with ["B", "C", "A"]. The fastest responses
land first, so row "a" shows the value for "b".

A batch with a single id works fine.
