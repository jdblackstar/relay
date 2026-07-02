# Apple container e2e test

This runs relay in an isolated Linux container using Apple's `container` CLI.

## Prereqs

- macOS 26 on Apple silicon
- `container` installed and `container system start` has been run once

## Run

```sh
./scripts/e2e-container.sh
```

This builds the image, runs the e2e test, and removes the container. The
image is also removed unless `KEEP_IMAGE=1` is set.

The e2e test exercises both `relay sync` and `relay watch` (by creating a
new command file and waiting for it to propagate).

## Cleanup

No manual cleanup is needed unless you set `KEEP_IMAGE=1`.

## Overrides

- `IMAGE_NAME=relay-e2e` and `CONTAINER_NAME=relay-e2e` to rename
- `KEEP_IMAGE=1` to skip deleting the image after the run
