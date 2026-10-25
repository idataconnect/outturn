# syntax=docker/dockerfile:1
# The UI in production: the built files on a static file server that also
# carries /v1 through to the API. Built from ui/ as the context, since the
# repository's own .dockerignore leaves this directory out of the tiers'.
#
#   docker build -t outturn-ui ui
#
# One image for every deployment. The name, mark, tagline and theme are read
# from the environment when the container starts (server/config.json), not
# compiled in; see src/lib/brand.ts.
FROM node:22 AS build
WORKDIR /ui
COPY package.json package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci
COPY . .
RUN npm run build

# Only for its binary, which is static. Copied onto distroless like the tiers,
# so the image has no shell or package manager under the server.
FROM caddy:2.11.7-alpine AS caddy

FROM gcr.io/distroless/static-debian13 AS ui
COPY --from=caddy /usr/bin/caddy /usr/local/bin/caddy
COPY server/Caddyfile /etc/caddy/Caddyfile
COPY server/config.json /srv/config/config.json
COPY --from=build /ui/dist /srv/www
# Where the server would keep its state if it kept any. It is told not to
# (persist_config off), but still looks for the directories at start; /tmp is
# the one writable place under a read-only root, an emptyDir in k8s/deploy.
ENV XDG_CONFIG_HOME=/tmp/caddy XDG_DATA_HOME=/tmp/caddy
# Where /v1 goes. The Service name in k8s/deploy; any host:port or URL works.
ENV OUTTURN_API_UPSTREAM=outturn-api:8080
EXPOSE 8080
USER 65534:65534
ENTRYPOINT ["caddy", "run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"]
