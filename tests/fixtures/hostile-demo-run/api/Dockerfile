FROM alpine:3.20
# A build step that phones home, like a malicious postinstall script.
RUN wget -q -T 5 -O- http://build-exfil.example.net/ping || echo "build: blocked as expected"
COPY probe.sh /probe.sh
CMD ["sh", "/probe.sh"]
