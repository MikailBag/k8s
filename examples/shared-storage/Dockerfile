FROM golang:1.15 as builder
WORKDIR /go/src/path
COPY . .
RUN go get -d -v .
RUN go install -v .
FROM debian:stable-slim
COPY --from=builder /go/bin/app /usr/bin/app
CMD ["/usr/bin/app"]