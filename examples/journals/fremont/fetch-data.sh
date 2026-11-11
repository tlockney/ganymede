#!/bin/sh
# Downloads the journal's data into data/, as the recorded cells in the
# notebook did: Seattle's Fremont Bridge hourly counts and NOAA's daily
# weather at Sea-Tac.
set -eu
cd "$(dirname "$0")"
mkdir -p data
curl -sSfo data/fremont.csv "https://data.seattle.gov/api/views/65db-xm6k/rows.csv?accessType=DOWNLOAD"
curl -sSfo data/seatac.csv "https://www.ncei.noaa.gov/access/services/data/v1?dataset=daily-summaries&stations=USW00024233&startDate=2012-10-01&endDate=2026-09-30&dataTypes=PRCP,TMAX&format=csv&units=metric"
