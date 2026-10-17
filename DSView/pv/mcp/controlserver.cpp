/*
 * This file is part of the DSView project.
 * DSView is based on PulseView.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 51 Franklin St, Fifth Floor, Boston, MA  02110-1301 USA
 */

#include "controlserver.h"

#include <QLocalServer>
#include <QLocalSocket>
#include <QJsonDocument>
#include <QJsonParseError>
#include <QPointer>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QStandardPaths>
#include "controlapi.h"
#include "../log.h"

namespace pv {
namespace mcp {

namespace {

// JSON-RPC 2.0 error codes.
const int ParseError = -32700;
const int InvalidRequest = -32600;
const int ServerError = -32000;

} // namespace

ControlServer::ControlServer(MainWindow *window, QObject *parent) :
    QObject(parent),
    _server(new QLocalServer(this)),
    _api(new ControlApi(window, this))
{
    connect(_server, &QLocalServer::newConnection, this, &ControlServer::on_new_connection);
}

ControlServer::~ControlServer()
{
    _server->close();
}

QString ControlServer::default_socket_path()
{
    QString custom = QString::fromLocal8Bit(qgetenv("DSVIEW_MCP_SOCKET"));
    if (!custom.isEmpty())
        return custom;

    QString base = QString::fromLocal8Bit(qgetenv("XDG_RUNTIME_DIR"));
    if (base.isEmpty() || !QDir(base).exists())
        base = QDir::homePath() + "/.local/state";
    return base + "/dsview/mcp.sock";
}

bool ControlServer::start()
{
    _path = default_socket_path();
    QString dir = QFileInfo(_path).absolutePath();

    // sockaddr_un.sun_path holds 108 bytes including the terminator.
    if (_path.toLocal8Bit().size() > 107) {
        dsv_err("MCP: socket path is too long (max 107 bytes): %s", _path.toUtf8().data());
        return false;
    }

    if (!QDir().mkpath(dir)) {
        dsv_err("MCP: cannot create socket directory %s", dir.toUtf8().data());
        return false;
    }
    QFile::setPermissions(dir, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner);

    // Another DSView may already serve this socket; never steal it.
    QLocalSocket probe;
    probe.connectToServer(_path);
    if (probe.waitForConnected(300)) {
        probe.disconnectFromServer();
        dsv_err("MCP: another DSView instance already listens on %s", _path.toUtf8().data());
        return false;
    }
    QLocalServer::removeServer(_path);

    _server->setSocketOptions(QLocalServer::UserAccessOption);
    if (!_server->listen(_path)) {
        dsv_err("MCP: cannot listen on %s: %s", _path.toUtf8().data(),
                _server->errorString().toUtf8().data());
        return false;
    }

    dsv_info("MCP: control socket ready at %s", _path.toUtf8().data());
    return true;
}

void ControlServer::on_new_connection()
{
    while (QLocalSocket *socket = _server->nextPendingConnection()) {
        connect(socket, &QLocalSocket::readyRead, this, &ControlServer::on_ready_read);
        connect(socket, &QLocalSocket::disconnected, socket, &QObject::deleteLater);
    }
}

void ControlServer::on_ready_read()
{
    QLocalSocket *socket = qobject_cast<QLocalSocket *>(sender());
    if (socket == NULL)
        return;

    while (socket->canReadLine()) {
        QByteArray line = socket->readLine(MaxLineBytes + 1);
        if (line.size() > MaxLineBytes) {
            send_error(socket, QJsonValue(), InvalidRequest, "request too large");
            socket->disconnectFromServer();
            return;
        }
        line = line.trimmed();
        if (!line.isEmpty())
            handle_line(socket, line);
    }

    if (socket->bytesAvailable() > MaxLineBytes) {
        send_error(socket, QJsonValue(), InvalidRequest, "request too large");
        socket->disconnectFromServer();
    }
}

void ControlServer::handle_line(QLocalSocket *socket, const QByteArray &line)
{
    QJsonParseError perr;
    QJsonDocument doc = QJsonDocument::fromJson(line, &perr);
    if (perr.error != QJsonParseError::NoError || !doc.isObject()) {
        send_error(socket, QJsonValue(), ParseError, "invalid JSON");
        return;
    }

    QJsonObject req = doc.object();
    QJsonValue id = req["id"];
    QString method = req["method"].toString();
    if (req["jsonrpc"].toString() != "2.0" || method.isEmpty() ||
        (req.contains("params") && !req["params"].isObject())) {
        send_error(socket, id, InvalidRequest, "expected a JSON-RPC 2.0 request with object params");
        return;
    }

    // The reply may come much later (captures, decoding); the socket can be gone by then.
    QPointer<QLocalSocket> target(socket);
    bool notification = !req.contains("id");

    _api->dispatch(method, req["params"].toObject(), [target, id, notification](const QJsonValue &result, const QString &error) {
        if (notification || target.isNull())
            return;
        if (!error.isEmpty()) {
            send_error(target, id, ServerError, error);
            return;
        }
        QJsonObject resp;
        resp["jsonrpc"] = "2.0";
        resp["id"] = id;
        resp["result"] = result;
        send(target, resp);
    });
}

void ControlServer::send(QLocalSocket *socket, const QJsonObject &msg)
{
    QByteArray data = QJsonDocument(msg).toJson(QJsonDocument::Compact);
    data.append('\n');
    socket->write(data);
    socket->flush();
}

void ControlServer::send_error(QLocalSocket *socket, const QJsonValue &id, int code, const QString &message)
{
    QJsonObject err;
    err["code"] = code;
    err["message"] = message;
    QJsonObject resp;
    resp["jsonrpc"] = "2.0";
    resp["id"] = id;
    resp["error"] = err;
    send(socket, resp);
}

} // namespace mcp
} // namespace pv
