# -*- coding: utf-8 -*-

import os
import sys
import urllib
import urllib.request  as urllib2 
from bs4 import BeautifulSoup
import json
import time
import rarfile
import zipfile

ZIMUKU_API = 'http://www.zimuku.la/search?q=%s'
ZIMUKU_BASE = 'http://www.zimuku.la'
UserAgent  = 'Mozilla/5.0 (compatible; MSIE 10.0; Windows NT 6.1; Trident/6.0)'
EXTS = [".srt", ".ass", ".rar", ".zip"]

def DebugLog(*msgs):
    if True:
        print('[DEBUG]: ', msgs)

def HtmlRead(url, retry=5):
    data = ''
    for i in range(retry):
        try:
            req = urllib2.Request(url)
            req.add_header('User-Agent', UserAgent)
            socket = urllib2.urlopen(req)
            data = socket.read()
            socket.close()
            break
        except:
            DebugLog('Reading html got error on line: %s, error: %s' % (sys.exc_info()[2].tb_lineno, sys.exc_info()[1]))
            time.sleep(30)
            DebugLog('After 30s, retry %d times.', i+1)
            continue
    return BeautifulSoup(data, 'html.parser')
    
#serch subtiles and find main page of this subtitle
def Search(name):
    subtitles_list = []
    DebugLog("Search for [%s] by name" % name)
    url = ZIMUKU_API % (urllib.parse.quote(name))
    DebugLog("Search API url: %s" % url)
    soup = HtmlRead(url)
    results = soup.find_all("div", class_="item prel clearfix")
    #print(results)

    for it in results:
        movie_name = it.find("div", class_="title").a.text.encode('utf-8')
        DebugLog('Movie name: ', movie_name.decode("utf-8"))
        movie_url = urllib.parse.urljoin(ZIMUKU_BASE, it.find("div", class_="title").a.get('href'))
        DebugLog('Movie url: ', movie_url)

        soup = HtmlRead(movie_url).find("div", class_="subs box clearfix")
        subs = soup.tbody.find_all("tr")
        for sub in subs:
            link = '%s%s' % (ZIMUKU_BASE, sub.a.get('href'))
            version = sub.a.text
            try:
                td = sub.find("td", class_="tac lang")
                r2 = td.find_all("img")
                langs = [x.get('title') for x in r2]
            except:
                langs = '未知'
            name = '%s (%s)' % (version, ",".join(langs))
            if ('English' in langs) and not(('简体中文' in langs) or ('繁體中文' in langs)):
                subtitles_list.append({"language_name":"English", "filename":name, "link":link, "language_flag":'en', "rating":"0", "lang":langs})
            else:
                subtitles_list.append({"language_name":"Chinese", "filename":name, "link":link, "language_flag":'zh', "rating":"0", "lang":langs})

    DebugLog("Sub titles:", len(subtitles_list))
    for it in subtitles_list:
        DebugLog(it)

    return subtitles_list

#select the right format and language of subtitle files
def SelectFile(rf):
    rf_list = rf.infolist()
    score_list = [0 for x in range(len(rf_list))]    
    for i, f in enumerate(rf_list):
        if not f.is_dir():
            score_list[i] += 1
        if '.ass' in f.filename:
            score_list[i] += 2
        if '.srt' in f.filename:
            score_list[i] += 3
        if 'eng' in f.filename:
            score_list[i] += 1
        if 'cht' in f.filename:
            score_list[i] += 2
        if 'chs' in f.filename:
            score_list[i] += 3
    at = score_list.index(max(score_list))
    return rf_list[at]

def UnzipAndClean(pkg_name, extension_name, path_to, new_name):
    if extension_name == '.rar':
        rf = rarfile.RarFile(pkg_name)
        f = SelectFile(rf)
        f.filename = os.path.basename(f.filename)
        DebugLog('Select subtitle file: %s, size: %d' % (f.filename, f.file_size))
        rf.extract(f, path_to)
        old_name = f.filename
    elif extension_name == '.zip':
        zf = zipfile.ZipFile(pkg_name)
        f = SelectFile(zf)
        f.filename = os.path.basename(f.filename)
        DebugLog('Select subtitle file: %s, size: %d' % (f.filename, f.file_size))
        ret = zf.extract(f, path_to)
        print(ret)
        old_name = f.filename
    elif extension_name == '.srt' or extension_name == '.ass':
        old_name = pkg_name

    #rename new file
    new_name += old_name[-4:]
    if os.path.exists(new_name):
        os.remove(new_name)
        DebugLog('Remove new file: %s' % new_name) 
    if os.path.exists(old_name):
        os.rename(old_name, new_name)
        DebugLog('Rename file: %s to %s' % (old_name, new_name))

    #delete old files
    if os.path.exists(old_name):
        os.remove(old_name)
        DebugLog('Remove old file: %s' % old_name)

    #delete pkg files
    if os.path.exists(pkg_name):
        os.remove(pkg_name)
        DebugLog('Remove pkg file: %s' % pkg_name)

    return True

#try to download for a single link
def DownloadOne(link, referer):
    url = link.get('href')
    if url[:4] != 'http':
        url = ZIMUKU_BASE + url        
    try:
        DebugLog('Trying to download: %s' % url)
        req = urllib2.Request(url)
        req.add_header('User-Agent', UserAgent)
        req.add_header('Referer', referer)
        socket = urllib2.urlopen(req)
        filename = socket.headers['Content-Disposition'].split('filename=')[1]
        if filename[0] == '"' or filename[0] == "'":
            filename = filename[1:-1]
        data = socket.read()
        socket.close()
        return filename, data
    except:
        DebugLog('Failed to download on line: %s, error: %s' % (sys.exc_info()[2].tb_lineno, sys.exc_info()[1]))
    return '', ''

#from main page to get every link and select one to downloa to a file
def Download(url, path_to, new_name):
    soup = HtmlRead(url)
    download_page = soup.find("li", class_="dlsub").a.get('href')
    DebugLog("Download page: ", download_page)
    soup_d_page = HtmlRead(download_page)
    links = soup_d_page.find("div", {"class":"clearfix"}).find_all('a')
    #print(links)
    for link in links:
        #trying to download
        filename, data = DownloadOne(link, url)
        #check size
        if len(data) < 1024:
            DebugLog('Download subtitle file size incorrect: %d' % len(data))
            continue
        DebugLog('Download subtitle file size: %d' % len(data))
        #check extension
        ext = os.path.splitext(filename)[1].lower()
        if not ext in EXTS:
            DebugLog('Download subtitle file extension unknown: %s' % filename)
            return False
        DebugLog('Download subtitle file name: %s' % filename)
        #write to a temporary file
        now = time.time()
        ts = time.strftime("%Y%m%d%H%M%S",time.localtime(now)) + str(int((now - int(now)) * 1000))
        sub_name = os.path.join(path_to, "subtitles%s%s" % (ts, os.path.splitext(filename)[1])).replace('\\','/')
        with open(sub_name, "wb") as f:
            f.write(data)
        f.close()
        DebugLog('Write subtitle file: %s' % sub_name)
        #unzip and delete unnecessary files
        if UnzipAndClean(sub_name, ext, path_to, new_name):
            return True

    return False

#there are always severl matched subtitles, return ordered links with priority
def SlectSubtitle(subtitles_list):
    ret_list = []
    if len(subtitles_list) > 0:
        ret_list.append(subtitles_list[0]['link'])
    # for sub in subtitles_list:
    #     ret_list.append(sub['link'])
    return ret_list

# if __name__ == '__main__':
#     UnzipAndClean('subtitles20201014161829288.zip', '.zip', './', 'Palm Springs 2020')
#     sys.exit(0)

#     argv_len = len(sys.argv)
#     if argv_len == 2:
#         name = str(sys.argv[1]).strip()
#         results = Search(name)
#         ordered_list = SlectSubtitle(results)
#         for item in ordered_list:
#             if Download(item, './', name):
#                 DebugLog('Succeed to download subtitle, all done.')
#                 sys.exit(0)
#         DebugLog('Failed to download subtitle, all done.')
    
#     sys.exit(0)