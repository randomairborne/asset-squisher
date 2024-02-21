#!/usr/bin/env bash
mkdir ./public
cd ./public/
for ((i = 0 ; i < 20 ; i++)); do
  echo "Downloading image ${i}"
  curl -JOL https://picsum.photos/2048/2048 2> /dev/null &
done
wait
echo "Downloaded all images"
cd ..
