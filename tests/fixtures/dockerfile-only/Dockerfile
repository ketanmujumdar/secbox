# A project with a Dockerfile and no Compose file. The first stage's port is
# not in the image that runs, so it must not be offered.
FROM node:22-bookworm-slim AS build
EXPOSE 9229
WORKDIR /app
COPY server.js .

FROM node:22-bookworm-slim
WORKDIR /app
COPY --from=build /app/server.js .
RUN <<EOF
echo "EXPOSE 2222 inside a heredoc is not an instruction"
EOF
EXPOSE 3000
EXPOSE 8080/tcp \
       5353/udp
# Hostile-looking entries: none of these may become a port or reach a shell.
EXPOSE ${DEBUG_PORT:-9230} 7000-7010 4444$(touch${IFS}/tmp/x) "80;rm" 99999
CMD ["node", "server.js"]
