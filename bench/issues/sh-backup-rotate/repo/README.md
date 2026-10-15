# backup-tools

Two small scripts: `bin/backup.sh` creates a timestamped tar.gz of a
directory, `bin/rotate.sh` prunes an archive directory down to the
newest N backups.

## Usage

    bin/backup.sh ./data ./backups
    bin/rotate.sh ./backups 5

## Tests

    bash tests/run_tests.sh
