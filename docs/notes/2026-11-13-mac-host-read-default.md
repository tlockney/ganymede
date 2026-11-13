## 2026-11-13, Mac host read default (tlockney/ganymede-ui#585)

The host API decision "default reads cover the whole root" rested on
the apps' root being their own documents. That holds on iOS only: the
Mac app serves `/`. The Mac app now passes the folders under its
Settings as `[host] read` (ganymede-ui#585), and docs/host-api.md says
so. The server's own default is unchanged.
