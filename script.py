#!/usr/bin/env python

#author     : Neil Zhang
#email      : neilzhangy@gmail.com
#version    : 2.2

#----change logs----
#
#
# 2016-11-16    v1.1    Delete finished tasks, delete none media files, convert media files name.
# 2016-12-05    v1.2    Delete all files less then 500MB.
# 2017-01-11    v1.3    Separate jobs into different folders.
# 2017-04-06    v1.4    Rewrite to delete none movie files and folder automatically. Rename movie files automatically.
# 2017-04-08    v1.5    Fixed a bug which got wrong result when adding tasks manally.
# 2017-04-14    v1.6    Fixed a bug which created a folder with 8 characters only.
# 2017-04-16    v1.7    Fixed a bug which exited when folder removed manally.
# 2018-09-23    v1.9    Use new piratebay url and ip address.
# 2018-09-23    v2.0    Get movies from top100 and most seeders.
# 2020-08-09    v2.1    Use custome ip and port.
# 2020-10-10    v2.2    Can work without python lib transmissionrpc; Add -p option to print out all data in the DB.


import sys
import os
import string
import sqlite3
import time
import shutil

USE_TRANSMISSION_RPC = True

try:
    import transmissionrpc
except:
    print 'Importing transmissionrpc failed, skip this function, only get list and put into DB.'
    USE_TRANSMISSION_RPC = False

USAGE = """Usage: ./download_hd_movies.py [-f]
    -f Use this option when first running this script
    -p Print out what's inside DB
"""
WEB_FILE = './web_data'
DB_FILE = './movies.db'
DOWN_FILE = './to_download'
TABLE_NAME = 'MOVIES'
FIRST_RUN = False
MOVIE_INFO = {}
CURL_CMD_TOP = 'curl -k --retry 10 --connect-timeout 20 --max-time 20 -o %s https://tpb.party/top/207' % WEB_FILE
CURL_CMD_SEEDERS = 'curl -k --retry 10 --connect-timeout 20 --max-time 20 -o %s https://tpb.party/browse/207/1/7/0' % WEB_FILE
MOVIE_FILE_THRESHOLD = 500*1024*1024
TRANSMISSION_IP = '192.168.0.127'
TRANSMISSION_PORT = 9999


def NameConvert(name):
    ret = []
    flag = True
    for c in list(name):
        if c.isalnum():
            ret.append(c)
            flag = True
        elif flag:
            ret.append(' ')
            flag = False

    return ''.join(ret).strip()
    
def DownloadFilter(name):
    localtime = time.localtime(time.time())
    this_year = str(localtime.tm_year)
    last_year = str(localtime.tm_year - 1)
    #print 'Filter: this year is %s, last year is %s' % (this_year, last_year)
    
    #time filter, only this year and last year
    ret = name.find(this_year)
    if -1 == ret:
        ret = name.find(last_year)
        if -1 == ret:
            return False    
        
    #1080 filter
    ret = name.find('1080')
    if -1 == ret:
        return False  
    
    
    return True

def LoadFromWeb(cursor, conn, cmd):
    print 'Running %s' % cmd
    ret = os.system(cmd)
    if 0 != ret:
        print 'Run cmd got an error:%d, exit.' % ret
        sys.exit(1)
    print 'Running cmd successfully.'
    
    print 'Analysing response...'
    if os.path.exists(WEB_FILE):
        with open(WEB_FILE, 'r') as f:
            data = f.read()
    
    pos = 0
    while(True):
        pos = data.find('<div class="detName">', pos+24)
        if -1 == pos:
            break
        name_start = data.find('">', pos+24)
        if -1 == name_start:
            continue
        name_end = data.find('</a>', name_start+2)
        if -1 == name_end:
            continue
        name = data[name_start+2:name_end]
        name = NameConvert(name)

        url_start = data.find('<a href="', name_end)
        if -1 == url_start:
            continue
        url_end = data.find('"', url_start+9)
        if -1 == url_end:
            continue
        url = data[url_start+9:url_end]

        ret = cursor.execute("SELECT * from %s where name = '%s'" % (TABLE_NAME, name))
        found = False
        for i in ret:
            if i[0] == name:
                found = True
                break
                
        if not found:
            if DownloadFilter(name):
                cursor.execute("INSERT INTO %s VALUES ('%s', '%s')" % (TABLE_NAME, name, url))
                conn.commit()
                print 'Found new movie [%s]' % name
                if FIRST_RUN:
                    pass
                else:            
                    MOVIE_INFO[name] = url

    print 'Analyse response successfully.'
        
def WriteToFile():
    print 'Writing tasks to file...'
    with open(DOWN_FILE, 'w') as f:
        for (k,v) in MOVIE_INFO.items():
            f.write(k + '\n')
            f.write(v + '\n')
            #print '[%s]' % k
    print 'Total number: %d' % len(MOVIE_INFO)
    
def DelOldTasks(tc, base_dir):
    print 'Delete old tasks...'
    torrents_list = tc.get_torrents()
    
    for torrent in torrents_list:
        print torrent.downloadDir
        if torrent.status=='seeding' or torrent.status=='stopped':
            hash = torrent.hashString
            path = torrent.downloadDir
            print 'Download dir is [%s], base dir is [%s]' % (path, base_dir)               
            try:
                if os.path.samefile(path, base_dir):
                    continue    
                dir_to_del = []
                for root, dirs, files in os.walk(path):
                    for file in files:
                        full_path = os.path.join(root, file)
                        ext = full_path[-3:].lower()
                        sz = os.path.getsize(full_path)
                        print 'File path is %s, size is %d' % (full_path, sz)
                        if (ext=='mkv' or ext=='mp4' or ext=='avi') and (sz > MOVIE_FILE_THRESHOLD):
                            new_file_name = NameConvert(file[:-4]) + '.' + ext
                            shutil.move(full_path, os.path.join(path, new_file_name))
                            print 'Move [%s] to [%s]' % (full_path, os.path.join(path, new_file_name))
                        else:   
                            os.remove(full_path)
                            print 'Remove %s' % full_path
                    for dir in dirs:
                        full_dir = os.path.join(root, dir)
                        dir_to_del.append(full_dir)
                        print 'Add %s to delete.' % full_dir
                        
                for i in dir_to_del:
                    shutil.rmtree(i,ignore_errors=True)
                    print 'Remove dir %s' % dir
            except:
                print 'Got error when trying to walk path %s' % path
                continue
            tc.remove_torrent(hash)
            print 'Remove torrent: %s' % hash
                
    print 'Delete done.'
    
def PostNewTasks(tc, base_dir):
    print 'Posting new tasks to transmission...'
    #i = 3   #only open when debuging
    for (k,v) in MOVIE_INFO.items():
        dir_to_down = os.path.join(base_dir, k).rstrip()
        print 'Creating download dir [%s]' % dir_to_down
        try:
            os.mkdir(dir_to_down)
        except:
            print 'Create download dir failed, error exit.'
            break
        ret = tc.add_torrent(v, download_dir=dir_to_down)
        print ret
        #i-=1    #only open when debuging
        #if i==0:    #only open when debuging
        #    break
    print 'Post done.'
    
def TackleTransmission():
    print 'Tackling transmissions...'
    tc = transmissionrpc.Client(TRANSMISSION_IP, port=TRANSMISSION_PORT)
    session = tc.get_session()
    download_dir = session.download_dir
    print 'Download dir is:%s' % download_dir
    if True==os.path.exists(download_dir):
        DelOldTasks(tc, download_dir)
        PostNewTasks(tc, download_dir)
    else:
        print 'Download dir does not exist, error exit.'
    del session
    del tc
    
def DbInit():
    print 'Connecting to DB...'
    conn = sqlite3.connect(DB_FILE)
    if conn is None:
        print 'Connect to db failed, exit.'
        sys.exit(1)
        
    cursor = conn.cursor()
    if cursor is None:
        print 'Failed to get cursor, exit.'
        sys.exit(1)
 
    found = False
    ret = cursor.execute("SELECT tbl_name FROM sqlite_master WHERE type='table'")
    for table_name in ret:
        if table_name[0] == TABLE_NAME:
            found = True
            break
            
    if not found:
        ret = cursor.execute("CREATE TABLE %s (`name` TEXT PRIMARY KEY NOT NULL, `url` TEXT DEFAULT NULL)" % TABLE_NAME)
        if ret is None:
            print 'Failed to create table, exit.'
            sys.exit(1)
            
    if found:
        ret = cursor.execute("select count(*) from %s" % TABLE_NAME)
        if ret is None:
            print 'Failed to get count, exit.'
            sys.exit(1)
        for i in ret:
            print 'Total items in database is %s' % i[0]
    
    print 'Connect to DB successfully.'
    return conn, cursor
    
def DbDeInit(conn, cursor):
    try:
        if cursor is not None:
            cursor.close()
    finally:
        if conn is not None:
            conn.close()
    print 'Cleanup done.'

def PrintDB():
    #init
    conn, cursor = DbInit()

    #print all
    counter = 0
    ret = cursor.execute("SELECT name from %s" % TABLE_NAME)
    for i in ret:
        print '%d exist movie [%s]' % (counter, i[0])
        counter += 1

    #clean up
    DbDeInit(conn, cursor)

if __name__ == '__main__':
    argv_len = len(sys.argv)
    if argv_len > 2:
        sys.stderr.write(USAGE)
        sys.exit(1)
        
    if (argv_len == 2 and sys.argv[1].strip().lower() == '-h'):
        sys.stderr.write(USAGE)
        sys.exit(1)
        
    if (argv_len == 2 and sys.argv[1].strip().lower() == '-f'):
        FIRST_RUN = True

    if (argv_len == 2 and sys.argv[1].strip().lower() == '-p'):
        PrintDB()
        sys.exit(0)
        
    #init
    conn, cursor = DbInit()
    
    #load from web
    LoadFromWeb(cursor, conn, CURL_CMD_TOP)
    LoadFromWeb(cursor, conn, CURL_CMD_SEEDERS)
    
    #write movies name and url to file
    WriteToFile()
    
    #tackle transmission
    if USE_TRANSMISSION_RPC:
        TackleTransmission()
    
    #clean up
    DbDeInit(conn, cursor)
    
    sys.exit(0)




