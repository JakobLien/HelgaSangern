import json

# Enkel liten script fil som generere en liste av romnavnan gitt en json henta fra søkefeltet på rombooking sida. 

rooms = {}

with open('rooms.json', 'r') as f:
    allRooms = json.loads(f.read())

    for room in allRooms:
        if room['building_name'] != 'Helgasetr':
            continue

        if room['can_book'] == False:
            continue

        if room['size'] < 10:
            continue

        rooms[room['name']] = room['size']

for room in reversed(rooms):
    if rooms[room] < 16:
        print(f'\t"{room}",')

