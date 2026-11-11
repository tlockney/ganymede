#!/bin/sh
# Downloads the journal's data into data/, as the recorded cells in the
# notebook did: USGS earthquake catalogs for Oklahoma and the Oklahoma
# Corporation Commission's yearly injection volumes: about 55 MB, most of
# it the workbooks.
set -eu
cd "$(dirname "$0")"
mkdir -p data/occ
box="minlatitude=33.6&maxlatitude=37.5&minlongitude=-100&maxlongitude=-94.4"
curl -sSfo data/ok-m25.csv "https://earthquake.usgs.gov/fdsnws/event/1/query?format=csv&starttime=2000-01-01&endtime=2026-11-01&$box&minmagnitude=2.5&orderby=time-asc"
for net in us ok; do
  curl -sSfo "data/$net-m15.csv" "https://earthquake.usgs.gov/fdsnws/event/1/query?format=csv&starttime=2019-01-01&endtime=2026-11-01&$box&minmagnitude=1.5&orderby=time-asc&catalog=$net"
done
for y in $(seq 2011 2025); do
  curl -sSfLo "data/occ/$y.xlsx" "https://oklahoma.gov/content/dam/ok/en/occ/documents/og/ogdatafiles/$y-uic-injection-volumes.xlsx"
done
