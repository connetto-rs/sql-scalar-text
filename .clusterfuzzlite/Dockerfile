FROM gcr.io/oss-fuzz-base/base-builder-rust
COPY . $SRC/sql-values
WORKDIR $SRC/sql-values
COPY ./.clusterfuzzlite/build.sh $SRC/
