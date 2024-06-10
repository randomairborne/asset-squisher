# asset-squisher

asset-squisher is a Docker container and application to
ease the compression of static assets, such as scripts and
images. It's super easy to use!

```dockerfile
FROM ghcr.io/randomairborne/asset-squisher AS compressor

COPY /your-app/dist/ /your-app/raw-dist/

RUN asset-squisher /your-app/raw-dist/ /your-app/static/

FROM alpine:latest

COPY --from=built /executables/your-app /usr/bin/
COPY --from=compressor /your-app/static/ /var/www/your-app-static/

CMD ["/usr/bin/your-app"]
```

This example assumes your app will serve static files from
`/var/www/your-app-static/`. It will change all image files
to `png`, `jpeg`, `webp`, and `avif`, as well as copying the
original image files. Generic files, like JavaScript files,
are copied into the new directory, along with .br (brotli),
.gz (gzip), .zz (deflate), and .zst (zstandard) variants which
are used by some web servers for precompression. For example,
if my input included `analytics.js`, files would be created for
`analytics.js`, `analytics.js.br`, `analytics.js.gz`, and so on.

asset-squisher tends to take a while! If you have a lot of assets, be prepared, and consider disabling encodings you
don't need.