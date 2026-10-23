FROM ghcr.io/nginx/nginx-unprivileged:stable-alpine
COPY nginx.conf /etc/nginx/nginx.conf
COPY index.html app.js style.css config.json /usr/share/nginx/html/
USER 1000:1000
EXPOSE 8080
ENTRYPOINT ["nginx"]
CMD ["-g", "daemon off;"]
