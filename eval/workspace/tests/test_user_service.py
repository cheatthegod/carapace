"""Tests for user_service — some pass, some fail."""

import os
import sqlite3
import sys
import tempfile

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

import user_service


def setup_db(tmp_path):
    db_path = os.path.join(tmp_path, "test.db")
    os.environ["USER_DB"] = db_path
    user_service.DB_PATH = type(user_service.DB_PATH)(db_path)
    conn = sqlite3.connect(db_path)
    conn.execute("""
        CREATE TABLE users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT UNIQUE NOT NULL,
            password_hash TEXT NOT NULL,
            role TEXT DEFAULT 'user'
        )
    """)
    conn.commit()
    conn.close()
    return db_path


def test_create_and_get_user():
    with tempfile.TemporaryDirectory() as tmp:
        setup_db(tmp)
        uid = user_service.create_user("alice", "password123")
        assert uid == 1
        user = user_service.get_user("alice")
        assert user is not None
        assert user["username"] == "alice"
        assert user["role"] == "user"


def test_password_verification():
    with tempfile.TemporaryDirectory() as tmp:
        setup_db(tmp)
        user_service.create_user("bob", "secret")
        user = user_service.get_user("bob")
        assert user_service.verify_password("secret", user["password_hash"])
        assert not user_service.verify_password("wrong", user["password_hash"])


def test_list_users():
    with tempfile.TemporaryDirectory() as tmp:
        setup_db(tmp)
        user_service.create_user("u1", "p1")
        user_service.create_user("u2", "p2")
        users = user_service.list_users()
        assert len(users) == 2


def test_promote_to_admin():
    with tempfile.TemporaryDirectory() as tmp:
        setup_db(tmp)
        user_service.create_user("charlie", "pw")
        assert user_service.promote_to_admin("charlie")
        user = user_service.get_user("charlie")
        assert user["role"] == "admin"


def test_delete_user():
    with tempfile.TemporaryDirectory() as tmp:
        setup_db(tmp)
        user_service.create_user("dave", "pw")
        assert user_service.delete_user("dave")
        assert user_service.get_user("dave") is None


if __name__ == "__main__":
    for name, func in list(globals().items()):
        if name.startswith("test_") and callable(func):
            try:
                func()
                print(f"  PASS {name}")
            except Exception as e:
                print(f"  FAIL {name}: {e}")
