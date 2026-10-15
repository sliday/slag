# Rotation deleted my newest backups

I run `bin/rotate.sh ./backups 2` every night to keep the two most
recent archives. This morning the two newest backups were gone and the
directory still held archives from the start of the month. We nearly
lost a day of data.

Expected: rotation keeps the newest archives by modification time and
deletes the older ones.

Actual: rotation deletes the newest archives and keeps the old ones.

To reproduce: create five backups across five days, run the rotation
with KEEP=2, then look at which files survive. The survivors are the
two oldest.
