# Quickstart Guide

This guide will walk you through the basics of getting a Caqui project up and running.

## Step 1: Initialize Project
Start by initializing a new Caqui project in your current directory:
```bash
caqui init
```
This will set up the basic directory structure and configuration files you need.

## Step 2: Write a basic `schema.cq`
Next, define your data models using the Caqui Schema language. Open `schema.cq` and add the following example:
```
model User {
  id    ID     @id
  name  String
  email String @unique
  posts [Post]
}

model Post {
  id       ID     @id
  title    String
  content  String
  author   User   @relation
}
```

## Step 3: Sync to database
Push your schema to the database. This will create the necessary tables and relationships:
```bash
caqui schema push
```

## Step 4: Start the server and query
Finally, start the Caqui API server:
```bash
caqui api start
```
You can now start sending requests to the API!
