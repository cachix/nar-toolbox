# nar-toolbox

An experimental tool to ease life working with Nix ARchive (NAR) files.

### Usage

Spin up a server to fetch files from caches using either a hash or a full store path.

Best used for small NARs, as fetching a file requires parsing the NAR from the beginning until the requested file is reached.

```console
nar-toolbox serve https://cache.nixos.org
```

```
curl --location http://localhost:8080/nix/store/<hash>-<name>/foo/bar.txt
```
