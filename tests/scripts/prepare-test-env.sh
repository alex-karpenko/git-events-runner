#!/bin/bash

# 1 - files prefix
#     may include path to destination folder
#     or may be used to generate multiple bundles at the same location

prefix="${1}"
basedir=$(dirname ${0})
gitea_bare_dir=$(realpath ${basedir}/../gitea/bare)
gitea_ssh_dir=$(realpath ${basedir}/../gitea/ssh)

if [[ -z "${prefix}" ]]; then
    prefix="tests/"
fi

openssl req -nodes -x509 -days 3650 -sha256 -batch -subj "/CN=Test RSA root CA" \
            -newkey rsa:4096 -keyout ${prefix}ca.key -out ${prefix}ca.crt

openssl req -nodes -sha256 -batch -subj "/CN=Test RSA intermediate CA" \
            -newkey rsa:3072 -keyout ${prefix}inter.key -out ${prefix}inter.req

openssl req -nodes -sha256 -batch -subj "/CN=test-server.com" \
            -newkey rsa:2048 -keyout ${prefix}end.key -out ${prefix}end.req

openssl rsa -in ${prefix}end.key -out ${prefix}test-server.key

openssl x509 -req -sha256 -days 3650 -set_serial 123 -extensions v3_inter -extfile ${basedir}/openssl.cnf \
             -CA ${prefix}ca.crt -CAkey ${prefix}ca.key -in ${prefix}inter.req -out ${prefix}inter.crt

openssl x509 -req -sha256 -days 2000 -set_serial 456 -extensions v3_end -extfile ${basedir}/openssl.cnf \
             -CA ${prefix}inter.crt -CAkey ${prefix}inter.key -in ${prefix}end.req -out ${prefix}end.crt

rm -rf ${prefix}tls ${prefix}gitea-runtime
mkdir -p ${prefix}tls ${prefix}ssh ${prefix}gitea-runtime ${prefix}gitea-runtime/config/ssl

cat ${prefix}end.crt ${prefix}inter.crt > ${prefix}tls/test-server.pem
cat ${prefix}inter.crt ${prefix}ca.crt > ${prefix}tls/ca.pem
cp ${prefix}end.key ${prefix}tls/test-server.key
cp ${prefix}end.crt ${prefix}tls/

rm ${prefix}*.req ${prefix}*.crt ${prefix}*.key

cp -R ${gitea_bare_dir}/* ${prefix}gitea-runtime/
cp ${gitea_ssh_dir}/* ${prefix}ssh/
cp ${prefix}tls/test-server.* ${prefix}gitea-runtime/config/ssl

if [[ ${CI} == "true" ]]; then
    sudo chown -R 1000:1000 ${prefix}gitea-runtime
fi
